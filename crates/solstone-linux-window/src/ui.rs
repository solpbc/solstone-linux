// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use gtk::prelude::*;
use solstone_linux::copy::CopyKey;
use solstone_linux::dbus_client::ControlClient;

use crate::controller::{
    Controller, IntakeCommand, PairingCommand, SettingsDraft, SettingsWriteOutcome,
};
use crate::copy_map::{
    ServicePresenceKind, map_fault, map_intake, map_journal_health, map_open_journal_outcome,
    map_pairing, map_service_start, map_settings_state, validate_pairing_link,
    validate_settings_changes,
};
use crate::format::{format_integer, format_timestamp, parse_integer};
use crate::logs::{collect_recent_logs, save_frozen_log};

type Changed = Rc<RefCell<Option<Rc<dyn Fn()>>>>;
fn notify(changed: &Changed) {
    let update = changed.borrow().clone();
    if let Some(update) = update {
        update();
    }
}

pub fn build_ui(app: &gtk::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(CopyKey::WindowTitle.as_str())
        .default_width(600)
        .default_height(620)
        .build();

    window.set_size_request(460, 520);

    let notebook = gtk::Notebook::new();

    let controller = Rc::new(RefCell::new(Controller::new()));
    let changed: Changed = Rc::new(RefCell::new(None));

    // Page 1: Status
    let (status_page, update_status) =
        create_status_page(Rc::clone(&controller), Rc::clone(&changed));
    notebook.append_page(
        &status_page,
        Some(&gtk::Label::new(Some(CopyKey::NavStatus.as_str()))),
    );

    // Page 2: Journal
    let (journal_page, update_journal) =
        create_journal_page(Rc::clone(&controller), Rc::clone(&changed));
    notebook.append_page(
        &journal_page,
        Some(&gtk::Label::new(Some(CopyKey::NavJournal.as_str()))),
    );

    // Page 3: Settings
    let (settings_page, update_settings) =
        create_settings_page(Rc::clone(&controller), Rc::clone(&changed));
    notebook.append_page(
        &settings_page,
        Some(&gtk::Label::new(Some(CopyKey::NavSettings.as_str()))),
    );

    // Page 4: Help
    let help_page = create_help_page(Rc::clone(&controller), &window);
    notebook.append_page(
        &help_page,
        Some(&gtk::Label::new(Some(CopyKey::NavHelp.as_str()))),
    );

    window.set_child(Some(&notebook));
    window.present();

    let update_all: Rc<dyn Fn()> = {
        let update_status = update_status.clone();
        let update_journal = update_journal.clone();
        let update_settings = update_settings.clone();
        Rc::new(move || {
            update_status();
            update_journal();
            update_settings();
        })
    };

    *changed.borrow_mut() = Some(Rc::clone(&update_all));
    update_all();

    // Kick off async poll loop
    spawn_poll_loop(Rc::clone(&controller), Rc::clone(&update_all));
}

async fn refresh_state(controller: &Rc<RefCell<Controller>>) {
    {
        let mut c = controller.borrow_mut();
        if c.refreshing {
            return;
        }
        c.refreshing = true;
    }
    use solstone_linux::service::{classify_user_service, user_service_pid};
    let id = controller.borrow_mut().alloc_id();
    let service = gio::spawn_blocking(|| {
        let runner = solstone_linux::cli::SystemRunner;
        (
            classify_user_service(&runner),
            user_service_pid(&runner).ok(),
        )
    })
    .await;
    let (presence, service_pid) =
        service.unwrap_or((solstone_linux::service::ServicePresence::Uncertain, None));
    let result = match ControlClient::session().await {
        Ok(client) => match client.get_owner().await {
            Ok(owner) => {
                let pid = client.get_connection_unix_process_id(&owner).await.ok();
                let reply = client.expect_owner(owner.clone()).get_control_state().await;
                Some((owner, pid, reply))
            }
            Err(_) => None,
        },
        Err(_) => None,
    };
    let mut c = controller.borrow_mut();
    c.service_presence = presence;
    if let Some((owner, pid, reply)) = result {
        c.observe_service_reply(id, &owner, pid, service_pid, reply);
    } else {
        c.set_bus_owner(None);
    }
    let now = std::time::Instant::now();
    if c.start_deadline.is_some_and(|deadline| now >= deadline) {
        c.start = None;
        c.start_deadline = None;
        c.start_error = Some("unavailable".into());
    }
    if c.restart_expected
        .as_ref()
        .is_some_and(|(_, _, _, deadline)| now >= *deadline)
    {
        c.restart = None;
        c.restart_expected = None;
        c.restart_error = Some("unavailable".into());
    }
    c.expire_intake(now);
    c.refreshing = false;
}

fn spawn_poll_loop(controller: Rc<RefCell<Controller>>, update_all: Rc<dyn Fn()>) {
    let busy = Rc::new(Cell::new(false));
    let refresh = move || {
        if busy.replace(true) {
            return glib::ControlFlow::Continue;
        }
        let ctrl = Rc::clone(&controller);
        let upd = Rc::clone(&update_all);
        let busy = Rc::clone(&busy);
        glib::MainContext::default().spawn_local(async move {
            refresh_state(&ctrl).await;
            busy.set(false);
            upd();
        });
        glib::ControlFlow::Continue
    };
    refresh();
    glib::timeout_add_local(Duration::from_secs(2), refresh);
}

fn begin_start(controller: Rc<RefCell<Controller>>, changed: Changed) {
    if controller.borrow().start.is_some() {
        return;
    }
    let id = controller.borrow_mut().start_service_start("");
    controller.borrow_mut().start_deadline =
        Some(std::time::Instant::now() + Duration::from_secs(45));
    notify(&changed);
    glib::MainContext::default().spawn_local(async move {
        use solstone_linux::dbus_client::ControlClientError;
        use solstone_linux::service::{ServicePaths, WindowStart, start_for_window};
        let absent = match ControlClient::session().await {
            Ok(client) => matches!(client.get_owner().await, Err(ControlClientError::NoOwner)),
            Err(_) => false,
        };
        let (result, diagnostic) = if absent {
            gio::spawn_blocking(|| {
                let mut output = Vec::new();
                let result = match ServicePaths::for_window() {
                    Ok(paths) => {
                        start_for_window(&paths, &solstone_linux::cli::SystemRunner, &mut output)
                    }
                    Err(error) => return (WindowStart::Failed, error.to_string()),
                };
                (result, String::from_utf8_lossy(&output).into_owned())
            })
            .await
            .unwrap_or((WindowStart::Uncertain, String::new()))
        } else {
            (WindowStart::Uncertain, String::new())
        };
        {
            let mut c = controller.borrow_mut();
            if c.start.as_ref().is_some_and(|flight| flight.id == id)
                && result != WindowStart::Started
            {
                c.start = None;
                c.start_deadline = None;
                c.service_diagnostic = (!diagnostic.is_empty()).then_some(diagnostic);
                c.start_error = Some(
                    if result == WindowStart::Failed {
                        "failed"
                    } else {
                        "unavailable"
                    }
                    .into(),
                );
            }
        }
        notify(&changed);
    });
}

fn begin_restart(controller: Rc<RefCell<Controller>>, changed: Changed) {
    let (owner, expected_pid, wanted) = {
        let c = controller.borrow();
        if c.restart.is_some()
            || c.settings_draft.is_dirty()
            || c.settings_draft.in_flight.is_some()
        {
            return;
        }
        let (Some(owner), Some(pid)) = (&c.bus_owner, c.service_owner_pid) else {
            return;
        };
        (
            owner.clone(),
            pid,
            (!c.older_daemon).then(|| c.document.settings.clone()),
        )
    };
    let id = controller.borrow_mut().start_service_restart(&owner);
    {
        let mut c = controller.borrow_mut();
        c.restart_verified = false;
        c.restart_expected = Some((
            owner.clone(),
            expected_pid,
            wanted,
            std::time::Instant::now() + Duration::from_secs(45),
        ));
    }
    notify(&changed);
    glib::MainContext::default().spawn_local(async move {
        use solstone_linux::service::{MatchingRestart, restart_matching_pid_guarded};
        let (result, diagnostic) = match ControlClient::session().await {
            Ok(client)
                if client.get_owner().await.as_deref() == Ok(owner.as_str())
                    && client.get_connection_unix_process_id(&owner).await == Ok(expected_pid) =>
            {
                let runtime = tokio::runtime::Handle::current();
                gio::spawn_blocking(move || {
                    let mut output = Vec::new();
                    let result = restart_matching_pid_guarded(
                        &solstone_linux::cli::SystemRunner,
                        expected_pid,
                        &mut output,
                        &|| runtime.block_on(client.get_owner()).as_deref() == Ok(owner.as_str()),
                    );
                    (result, String::from_utf8_lossy(&output).into_owned())
                })
                .await
                .unwrap_or((MatchingRestart::Uncertain, String::new()))
            }
            _ => (MatchingRestart::NotMatching, String::new()),
        };
        {
            let mut c = controller.borrow_mut();
            if c.restart.as_ref().is_some_and(|flight| flight.id == id)
                && result != MatchingRestart::Restarted
            {
                c.restart = None;
                c.restart_expected = None;
                c.service_diagnostic = (!diagnostic.is_empty()).then_some(diagnostic);
                c.restart_error = Some(
                    if result == MatchingRestart::Failed {
                        "failed"
                    } else {
                        "unavailable"
                    }
                    .into(),
                );
            }
        }
        notify(&changed);
    });
}

fn connect_refresh(button: &gtk::Button, controller: Rc<RefCell<Controller>>, changed: Changed) {
    button.connect_clicked(move |_| {
        let controller = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        glib::MainContext::default().spawn_local(async move {
            refresh_state(&controller).await;
            notify(&changed);
        });
    });
}

fn connect_open_journal(
    button: &gtk::Button,
    controller: Rc<RefCell<Controller>>,
    changed: Changed,
) {
    button.connect_clicked(move |_| {
        if controller.borrow().browser_request.is_some() {
            return;
        }
        let Some(owner) = controller.borrow().bus_owner.clone() else {
            return;
        };
        let id = {
            let mut c = controller.borrow_mut();
            let id = c.alloc_id();
            c.browser_error = None;
            c.browser_request = Some(crate::controller::InFlight {
                id,
                bus_owner: owner.clone(),
                payload: (),
            });
            id
        };
        notify(&changed);
        let ctrl = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        glib::MainContext::default().spawn_local(async move {
            let result = match ControlClient::session().await {
                Ok(client) => client.expect_owner(owner.clone()).open_journal().await,
                Err(error) => Err(error),
            };
            if ctrl.borrow().bus_owner.as_deref() == Some(owner.as_str())
                && ctrl
                    .borrow()
                    .browser_request
                    .as_ref()
                    .is_some_and(|request| request.id == id)
            {
                ctrl.borrow_mut().browser_request = None;
                ctrl.borrow_mut().browser_error = match result {
                    Ok(json) => map_open_journal_outcome(&json),
                    Err(_) => Some(CopyKey::BrowserOpenFailed),
                };
            }
            notify(&changed);
        });
    });
}

fn create_status_page(
    controller: Rc<RefCell<Controller>>,
    changed: Changed,
) -> (gtk::ScrolledWindow, Rc<dyn Fn()>) {
    let scrolled = gtk::ScrolledWindow::new();
    let container = gtk::Box::new(gtk::Orientation::Vertical, 16);
    container.set_margin_top(24);
    container.set_margin_bottom(24);
    container.set_margin_start(24);
    container.set_margin_end(24);

    let headline_label = gtk::Label::new(None);
    headline_label.set_halign(gtk::Align::Start);
    let detail_label = gtk::Label::new(None);
    detail_label.set_halign(gtk::Align::Start);
    detail_label.set_wrap(true);

    let action_button = gtk::Button::new();
    action_button.set_halign(gtk::Align::Start);
    action_button.set_visible(false);

    let start_button = gtk::Button::new();
    start_button.set_halign(gtk::Align::Start);
    start_button.set_visible(false);

    let start_disclosure = gtk::Label::new(None);
    start_disclosure.set_halign(gtk::Align::Start);
    start_disclosure.set_wrap(true);
    start_disclosure.set_visible(false);

    let open_button = gtk::Button::with_label(CopyKey::ActionOpenJournal.as_str());
    open_button.set_halign(gtk::Align::Start);
    connect_open_journal(&open_button, Rc::clone(&controller), Rc::clone(&changed));
    let service_notice = gtk::Label::new(None);
    service_notice.set_halign(gtk::Align::Start);
    service_notice.set_wrap(true);
    {
        let ctrl = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        start_button.connect_clicked(move |_| begin_start(Rc::clone(&ctrl), Rc::clone(&changed)));
    }

    let check_button = gtk::Button::with_label(CopyKey::ActionCheckStatus.as_str());
    check_button.set_halign(gtk::Align::Start);
    connect_refresh(&check_button, Rc::clone(&controller), Rc::clone(&changed));
    container.append(&check_button);
    let stats_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let queue_label = gtk::Label::new(None);
    queue_label.set_halign(gtk::Align::Start);
    let quarantine_label = gtk::Label::new(None);
    quarantine_label.set_halign(gtk::Align::Start);
    let delivery_label = gtk::Label::new(None);
    delivery_label.set_halign(gtk::Align::Start);
    let version_label = gtk::Label::new(None);
    version_label.set_halign(gtk::Align::Start);
    version_label.set_visible(false);

    stats_box.append(&queue_label);
    stats_box.append(&quarantine_label);
    stats_box.append(&delivery_label);
    stats_box.append(&version_label);

    let expander = gtk::Expander::new(Some(CopyKey::StatusDetails.as_str()));
    let fault_label = gtk::Label::new(None);
    fault_label.set_wrap(true);
    fault_label.set_halign(gtk::Align::Start);
    expander.set_child(Some(&fault_label));

    container.append(&headline_label);
    container.append(&detail_label);
    container.append(&action_button);
    container.append(&start_button);
    container.append(&start_disclosure);
    container.append(&service_notice);
    container.append(&open_button);
    container.append(&stats_box);
    container.append(&expander);
    scrolled.set_child(Some(&container));

    {
        let ctrl = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        action_button.connect_clicked(move |_| {
            if ctrl.borrow().intake_command.is_some() {
                return;
            }
            let (is_pause, owner) = {
                let c = ctrl.borrow();
                let is_p = c.document.capture != "paused";
                let o = c.bus_owner.clone();
                (is_p, o)
            };
            if let Some(owner) = owner {
                let cmd = if is_pause {
                    IntakeCommand::Pause(0)
                } else {
                    IntakeCommand::Resume
                };
                let id = ctrl.borrow_mut().start_intake_command(&owner, cmd);
                {
                    let ctrl = Rc::clone(&ctrl);
                    let changed = Rc::clone(&changed);
                    glib::timeout_add_local_once(Duration::from_secs(30), move || {
                        ctrl.borrow_mut().expire_intake(std::time::Instant::now());
                        notify(&changed);
                    });
                }
                notify(&changed);
                let c_inner = Rc::clone(&ctrl);
                let changed = Rc::clone(&changed);
                glib::MainContext::default().spawn_local(async move {
                    let res = match ControlClient::session().await {
                        Ok(client) => {
                            let client = client.expect_owner(owner.clone());
                            if is_pause {
                                client.pause(0).await
                            } else {
                                client.resume().await
                            }
                        }
                        Err(error) => Err(error),
                    };
                    c_inner.borrow_mut().apply_intake_reply(id, &owner, res);
                    notify(&changed);
                });
            }
        });
    }

    let update = {
        let ctrl = Rc::clone(&controller);
        Rc::new(move || {
            let c = ctrl.borrow();
            let intake_keys = map_intake(&c.document);

            if let Some(k) = intake_keys.headline {
                headline_label.set_text(k.as_str());
                headline_label.set_visible(true);
            } else {
                headline_label.set_visible(false);
            }

            if let Some(k) = intake_keys.detail {
                detail_label.set_text(k.as_str());
                detail_label.set_visible(true);
            } else {
                detail_label.set_visible(false);
            }

            if let Some(k) = intake_keys.action {
                action_button.set_label(k.as_str());
                action_button.set_visible(true);
            } else {
                action_button.set_visible(false);
            }

            let start_keys = map_service_start(
                if c.bus_owner.is_some() {
                    ServicePresenceKind::Active
                } else {
                    match c.service_presence {
                        solstone_linux::service::ServicePresence::Absent => {
                            ServicePresenceKind::Absent
                        }
                        solstone_linux::service::ServicePresence::Inactive => {
                            ServicePresenceKind::Inactive
                        }
                        solstone_linux::service::ServicePresence::Active => {
                            ServicePresenceKind::Active
                        }
                        solstone_linux::service::ServicePresence::Uncertain => {
                            ServicePresenceKind::Uncertain
                        }
                    }
                },
                c.start.is_some(),
                c.start_error.as_deref() == Some("failed"),
                c.start_error.as_deref() == Some("unavailable"),
            );
            if let Some(command) = &c.intake_command {
                action_button.set_label(
                    match command.payload {
                        IntakeCommand::Pause(_) => CopyKey::ActionPausing,
                        IntakeCommand::Resume => CopyKey::ActionResuming,
                    }
                    .as_str(),
                );
            }
            action_button.set_sensitive(c.intake_command.is_none() && c.bus_owner.is_some());
            open_button.set_sensitive(c.browser_request.is_none());
            open_button.set_visible(map_journal_health(&c.document).action_open.is_some());
            let notice = c.browser_error.or(c.intake_error_key).or(start_keys.status);
            service_notice.set_text(notice.map_or("", |key| key.as_str()));
            service_notice.set_visible(notice.is_some());
            start_button.set_sensitive(
                c.start.is_none()
                    && c.bus_owner.is_none()
                    && matches!(
                        c.service_presence,
                        solstone_linux::service::ServicePresence::Absent
                            | solstone_linux::service::ServicePresence::Inactive
                    ),
            );
            if let Some(k) = start_keys.action {
                start_button.set_label(k.as_str());
                start_button.set_visible(true);
            } else {
                start_button.set_visible(false);
            }

            if let Some(k) = start_keys.disclosure {
                start_disclosure.set_text(k.as_str());
                start_disclosure.set_visible(true);
            } else {
                start_disclosure.set_visible(false);
            }

            if let Some(q) = c.document.queue_count {
                queue_label.set_text(&solstone_linux::copy::value_copy(
                    solstone_linux::copy::ValueKey::Waiting,
                    &format_integer(q),
                ));
                queue_label.set_visible(true);
            } else {
                queue_label.set_visible(false);
            }

            if let Some(q) = c.document.quarantine_count {
                if q > 0 {
                    quarantine_label.set_text(&solstone_linux::copy::plural_copy(
                        solstone_linux::copy::PluralKey::Attention,
                        q as usize,
                        &format_integer(q),
                    ));
                    quarantine_label.set_visible(true);
                } else {
                    quarantine_label.set_visible(false);
                }
            } else {
                quarantine_label.set_visible(false);
            }

            if let Some(d) = &c.document.last_delivery {
                let at_str = match d.at {
                    Some(at) => format_timestamp(at),
                    None => solstone_linux::copy::NOT_YET.to_string(),
                };
                delivery_label.set_text(&solstone_linux::copy::value_copy(
                    solstone_linux::copy::ValueKey::LastDelivery,
                    &at_str,
                ));
                delivery_label.set_visible(true);
            } else {
                delivery_label.set_visible(false);
            }

            if let Some(v) = &c.document.daemon_version {
                version_label.set_text(&solstone_linux::copy::value_copy(
                    solstone_linux::copy::ValueKey::Version,
                    v,
                ));
                version_label.set_visible(true);
            } else {
                version_label.set_visible(false);
            }

            if let Some(diagnostic) = &c.service_diagnostic {
                fault_label.set_text(diagnostic);
                expander.set_visible(true);
            } else if let Some(key) = map_fault(&c.document) {
                fault_label.set_text(key.as_str());
                expander.set_visible(true);
            } else {
                fault_label.set_text("");
                expander.set_visible(false);
            }
        })
    };

    (scrolled, update)
}

fn create_journal_page(
    controller: Rc<RefCell<Controller>>,
    changed: Changed,
) -> (gtk::ScrolledWindow, Rc<dyn Fn()>) {
    let presented = Rc::new(RefCell::new(None::<crate::controller::PairingPresentation>));
    let rendered_mark = Rc::new(RefCell::new(
        None::<(
            Option<solstone_linux::window_control::MarkRenderDocument>,
            bool,
        )>,
    ));
    let scrolled = gtk::ScrolledWindow::new();
    let container = gtk::Box::new(gtk::Orientation::Vertical, 16);
    container.set_margin_top(24);
    container.set_margin_bottom(24);
    container.set_margin_start(24);
    container.set_margin_end(24);

    let status_label = gtk::Label::new(None);
    status_label.set_halign(gtk::Align::Start);
    status_label.set_wrap(true);

    let detail_label = gtk::Label::new(None);
    detail_label.set_halign(gtk::Align::Start);
    detail_label.set_wrap(true);

    let open_button = gtk::Button::with_label(CopyKey::ActionOpenJournal.as_str());
    open_button.set_halign(gtk::Align::Start);
    open_button.set_visible(false);

    let check_button = gtk::Button::with_label(CopyKey::PairingCheckStatus.as_str());
    check_button.set_halign(gtk::Align::Start);
    connect_refresh(&check_button, Rc::clone(&controller), Rc::clone(&changed));
    container.append(&check_button);
    let mark_container = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let pairing_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let link_entry = gtk::Entry::new();
    link_entry.set_placeholder_text(Some(CopyKey::JournalLinkPlaceholder.as_str()));
    let pair_button = gtk::Button::with_label(CopyKey::ActionPair.as_str());
    pair_button.set_halign(gtk::Align::Start);

    let pairing_notice = gtk::Label::new(None);
    pairing_notice.set_halign(gtk::Align::Start);
    pairing_notice.set_wrap(true);
    pairing_notice.set_visible(false);

    let link_label = gtk::Label::new(Some(CopyKey::JournalLinkLabel.as_str()));
    link_label.set_halign(gtk::Align::Start);
    link_label.set_mnemonic_widget(Some(&link_entry));
    let question = gtk::Label::new(Some(CopyKey::PairingQuestion.as_str()));
    question.set_wrap(true);
    question.set_halign(gtk::Align::Start);
    let question_detail = gtk::Label::new(Some(CopyKey::PairingQuestionDetail.as_str()));
    question_detail.set_wrap(true);
    question_detail.set_halign(gtk::Align::Start);
    let held_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    held_box.append(&question);
    held_box.append(&question_detail);
    let confirm_btn = gtk::Button::with_label(CopyKey::PairingConfirm.as_str());
    let reject_btn = gtk::Button::with_label(CopyKey::PairingReject.as_str());
    let cancel_btn = gtk::Button::with_label(CopyKey::PairingCancel.as_str());
    held_box.append(&confirm_btn);
    held_box.append(&reject_btn);
    held_box.append(&cancel_btn);
    held_box.set_visible(false);

    pairing_box.append(&link_label);
    pairing_box.append(&link_entry);
    pairing_box.append(&pair_button);
    pairing_box.append(&pairing_notice);
    pairing_box.append(&held_box);

    container.append(&status_label);
    container.append(&detail_label);
    container.append(&open_button);
    container.append(&mark_container);
    container.append(&pairing_box);
    scrolled.set_child(Some(&container));

    connect_open_journal(&open_button, Rc::clone(&controller), Rc::clone(&changed));

    {
        let ctrl = Rc::clone(&controller);
        let link_entry = link_entry.clone();
        let pairing_notice = pairing_notice.clone();
        let changed = Rc::clone(&changed);
        pair_button.connect_clicked(move |_| {
            if ctrl.borrow().pairing.is_some() {
                return;
            }
            let text = link_entry.text();
            match validate_pairing_link(&text) {
                Ok(valid_link) => {
                    pairing_notice.set_visible(false);
                    let owner = ctrl.borrow().bus_owner.clone();
                    if let Some(owner) = owner {
                        let id = ctrl
                            .borrow_mut()
                            .start_pairing(&owner, PairingCommand::Begin(valid_link.clone()));
                        notify(&changed);
                        let changed = Rc::clone(&changed);
                        let c_inner = Rc::clone(&ctrl);
                        glib::MainContext::default().spawn_local(async move {
                            let res = match ControlClient::session().await {
                                Ok(client) => {
                                    client
                                        .expect_owner(owner.clone())
                                        .begin_pairing(&valid_link)
                                        .await
                                }
                                Err(error) => Err(error),
                            };
                            c_inner.borrow_mut().apply_pairing_reply(id, &owner, res);
                            notify(&changed);
                        });
                    }
                }
                Err(err_key) => {
                    pairing_notice.set_text(err_key.as_str());
                    pairing_notice.set_visible(true);
                }
            }
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let presented = Rc::clone(&presented);
        let pairing_notice = pairing_notice.clone();
        let changed = Rc::clone(&changed);
        confirm_btn.connect_clicked(move |_| {
            let shown = presented.borrow().clone();
            if let Some(shown) = shown {
                let owner = shown.bus_owner.clone();
                let (id, command) = match ctrl.borrow_mut().answer_presented(&shown, true) {
                    Ok(request) => request,
                    Err(key) => {
                        pairing_notice.set_text(key.as_str());
                        pairing_notice.set_visible(true);
                        return;
                    }
                };
                let PairingCommand::Answer {
                    attempt_id: att_id,
                    pairing_id: pair_id,
                    ..
                } = command
                else {
                    return;
                };
                notify(&changed);
                let changed = Rc::clone(&changed);
                let c_inner = Rc::clone(&ctrl);
                glib::MainContext::default().spawn_local(async move {
                    let res = match ControlClient::session().await {
                        Ok(client) => {
                            client
                                .expect_owner(owner.clone())
                                .answer_pairing(&att_id, &pair_id, true)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    c_inner.borrow_mut().apply_pairing_reply(id, &owner, res);
                    notify(&changed);
                });
            }
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let presented = Rc::clone(&presented);
        let pairing_notice = pairing_notice.clone();
        let changed = Rc::clone(&changed);
        reject_btn.connect_clicked(move |_| {
            let shown = presented.borrow().clone();
            if let Some(shown) = shown {
                let owner = shown.bus_owner.clone();
                let (id, command) = match ctrl.borrow_mut().answer_presented(&shown, false) {
                    Ok(request) => request,
                    Err(key) => {
                        pairing_notice.set_text(key.as_str());
                        pairing_notice.set_visible(true);
                        return;
                    }
                };
                let PairingCommand::Answer {
                    attempt_id: att_id,
                    pairing_id: pair_id,
                    ..
                } = command
                else {
                    return;
                };
                notify(&changed);
                let changed = Rc::clone(&changed);
                let c_inner = Rc::clone(&ctrl);
                glib::MainContext::default().spawn_local(async move {
                    let res = match ControlClient::session().await {
                        Ok(client) => {
                            client
                                .expect_owner(owner.clone())
                                .answer_pairing(&att_id, &pair_id, false)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    c_inner.borrow_mut().apply_pairing_reply(id, &owner, res);
                    notify(&changed);
                });
            }
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let presented = Rc::clone(&presented);
        let pairing_notice = pairing_notice.clone();
        let changed = Rc::clone(&changed);
        cancel_btn.connect_clicked(move |_| {
            let shown = presented.borrow().clone();
            if let Some(shown) = shown {
                let owner = shown.bus_owner.clone();
                let (id, command) = match ctrl.borrow_mut().cancel_presented(&shown) {
                    Ok(request) => request,
                    Err(key) => {
                        pairing_notice.set_text(key.as_str());
                        pairing_notice.set_visible(true);
                        return;
                    }
                };
                let PairingCommand::Cancel(att_id) = command else {
                    return;
                };
                notify(&changed);
                let changed = Rc::clone(&changed);
                let c_inner = Rc::clone(&ctrl);
                glib::MainContext::default().spawn_local(async move {
                    let res = match ControlClient::session().await {
                        Ok(client) => {
                            client
                                .expect_owner(owner.clone())
                                .cancel_pairing(&att_id)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    c_inner.borrow_mut().apply_pairing_reply(id, &owner, res);
                    notify(&changed);
                });
            }
        });
    }

    let update = {
        let ctrl = Rc::clone(&controller);
        let presented = Rc::clone(&presented);
        Rc::new(move || {
            let c = ctrl.borrow();
            let journal_keys = map_journal_health(&c.document);

            if let Some(k) = journal_keys.status {
                status_label.set_text(k.as_str());
                status_label.set_visible(true);
            } else {
                status_label.set_visible(false);
            }

            if let Some(k) = journal_keys.detail {
                detail_label.set_text(k.as_str());
                detail_label.set_visible(true);
            } else {
                detail_label.set_visible(false);
            }

            open_button.set_sensitive(c.browser_request.is_none());
            open_button.set_visible(journal_keys.action_open.is_some());

            // Mark chip rendering
            let unverifiable = c.document.fault.as_deref() == Some("unverifiable")
                || c.document
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.state == "unverifiable");
            let mark = if unverifiable {
                None
            } else if c
                .document
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.state == "pending")
            {
                c.document.pending_mark.clone()
            } else {
                c.document.confirmed_mark.clone()
            };
            let unavailable = c.document.durable != "unpaired"
                || c.document.attempt.is_some()
                || c.document.view == solstone_linux::window_control::SessionView::Unknown;
            let mark_state = Some((mark.clone(), unavailable));
            if *rendered_mark.borrow() != mark_state {
                while let Some(child) = mark_container.first_child() {
                    mark_container.remove(&child);
                }
                if let Some(mark) = mark.clone() {
                    mark_container.append(&crate::mark_card::create_mark_card_widget(mark));
                } else {
                    mark_container.append(&crate::mark_card::create_empty_mark_widget(unavailable));
                }
                *rendered_mark.borrow_mut() = mark_state;
            }
            mark_container.set_visible(true);

            let pairing_keys = map_pairing(&c.document, c.pairing_attempt_outcome.as_deref());
            let notice = c.browser_error.or(pairing_keys.title);
            pairing_notice.set_text(notice.map_or("", |key| key.as_str()));
            pairing_notice.set_visible(notice.is_some());
            question.set_visible(pairing_keys.secondary_title.is_some());
            question_detail.set_visible(pairing_keys.secondary_detail.is_some());
            if let Some(key) = pairing_keys.detail {
                detail_label.set_text(key.as_str());
                detail_label.set_visible(true);
            }

            held_box.set_visible(
                pairing_keys.confirm_action.is_some() || pairing_keys.reject_action.is_some(),
            );
            confirm_btn.set_visible(pairing_keys.confirm_action.is_some());
            reject_btn.set_visible(pairing_keys.reject_action.is_some());
            cancel_btn.set_visible(pairing_keys.cancel_action.is_some());
            let presentation = c.pairing_presentation();
            confirm_btn.set_label(
                if c.pairing.as_ref().is_some_and(|command| {
                    matches!(command.payload, PairingCommand::Answer { accept: true, .. })
                }) {
                    CopyKey::PairingConfirming
                } else {
                    CopyKey::PairingConfirm
                }
                .as_str(),
            );
            confirm_btn.set_sensitive(
                presentation
                    .as_ref()
                    .is_some_and(|shown| shown.mark.is_some()),
            );
            reject_btn.set_sensitive(presentation.is_some());
            cancel_btn.set_sensitive(presentation.is_some());
            pair_button.set_sensitive(
                c.pairing.is_none()
                    && c.bus_owner.is_some()
                    && c.document.view != solstone_linux::window_control::SessionView::Unknown,
            );
            pair_button.set_label(
                if c.pairing.is_some() {
                    CopyKey::ActionPairing
                } else {
                    CopyKey::ActionPair
                }
                .as_str(),
            );
            *presented.borrow_mut() = presentation;
        })
    };

    (scrolled, update)
}

fn split_exclusion_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn join_exclusion_list(items: &[String]) -> String {
    items.join("\n")
}

fn exclusion_text_view() -> gtk::TextView {
    let view = gtk::TextView::new();
    view.set_accepts_tab(false);
    view.set_wrap_mode(gtk::WrapMode::None);
    view
}

fn text_view_text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string()
}

fn set_text_view_text(view: &gtk::TextView, text: &str) {
    let buffer = view.buffer();
    let current = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
    if current.as_str() != text {
        buffer.set_text(text);
    }
}

fn activity_route_line(
    status: Option<&solstone_linux::window_control::RouteStatusModel>,
) -> String {
    let route = status
        .and_then(|item| item.route.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unavailable".into());
    let foreground = status
        .map(|item| item.foreground.as_str())
        .unwrap_or("unavailable");
    let title = status
        .map(|item| item.title.as_str())
        .unwrap_or("unavailable");
    let url = status
        .map(|item| item.url.as_str())
        .unwrap_or("unavailable");
    let idle = status
        .map(|item| item.idle.as_str())
        .unwrap_or("unavailable");
    format!("{route}; foreground {foreground}; title {title}; url {url}; idle {idle}")
}

fn create_settings_page(
    controller: Rc<RefCell<Controller>>,
    changed: Changed,
) -> (gtk::ScrolledWindow, Rc<dyn Fn()>) {
    let rendering = Rc::new(Cell::new(false));
    let scrolled = gtk::ScrolledWindow::new();
    let container = gtk::Box::new(gtk::Orientation::Vertical, 16);
    container.set_margin_top(24);
    container.set_margin_bottom(24);
    container.set_margin_start(24);
    container.set_margin_end(24);

    let load_failed_label = gtk::Label::new(Some(CopyKey::SettingsLoadFailed.as_str()));
    load_failed_label.set_halign(gtk::Align::Start);
    let reload_button = gtk::Button::with_label(CopyKey::SettingsReload.as_str());
    reload_button.set_halign(gtk::Align::Start);

    let form_box = gtk::Box::new(gtk::Orientation::Vertical, 12);

    let framerate_entry = gtk::Entry::new();
    let framerate_help = gtk::Label::new(Some(CopyKey::SettingsFrameRateHelp.as_str()));
    framerate_help.set_halign(gtk::Align::Start);

    let cursor_switch = gtk::Switch::new();
    cursor_switch.set_halign(gtk::Align::Start);
    let cursor_label = gtk::Label::new(Some(CopyKey::SettingsCursor.as_str()));
    cursor_label.set_halign(gtk::Align::Start);

    let start_paused_switch = gtk::Switch::new();
    start_paused_switch.set_halign(gtk::Align::Start);
    let start_paused_label = gtk::Label::new(Some(CopyKey::SettingsStartPaused.as_str()));
    start_paused_label.set_halign(gtk::Align::Start);

    let segment_entry = gtk::Entry::new();
    let segment_help = gtk::Label::new(Some(CopyKey::SettingsSegmentHelp.as_str()));
    segment_help.set_halign(gtk::Align::Start);

    let activity_explain = gtk::Label::new(Some(CopyKey::SettingsActivityExplain.as_str()));
    activity_explain.set_halign(gtk::Align::Start);
    activity_explain.set_wrap(true);
    let activity_source_switch = gtk::Switch::new();
    activity_source_switch.set_halign(gtk::Align::Start);
    let activity_source_label = gtk::Label::new(Some(CopyKey::SettingsActivitySource.as_str()));
    activity_source_label.set_halign(gtk::Align::Start);
    activity_source_label.set_mnemonic_widget(Some(&activity_source_switch));
    let activity_through_pause_switch = gtk::Switch::new();
    activity_through_pause_switch.set_halign(gtk::Align::Start);
    let activity_through_pause_label =
        gtk::Label::new(Some(CopyKey::SettingsActivityThroughPause.as_str()));
    activity_through_pause_label.set_halign(gtk::Align::Start);
    activity_through_pause_label.set_mnemonic_widget(Some(&activity_through_pause_switch));
    let activity_through_pause_help =
        gtk::Label::new(Some(CopyKey::SettingsActivityThroughPauseHelp.as_str()));
    activity_through_pause_help.set_halign(gtk::Align::Start);
    activity_through_pause_help.set_wrap(true);
    let activity_titles_switch = gtk::Switch::new();
    activity_titles_switch.set_halign(gtk::Align::Start);
    let activity_titles_label = gtk::Label::new(Some(CopyKey::SettingsActivityTitles.as_str()));
    activity_titles_label.set_halign(gtk::Align::Start);
    activity_titles_label.set_mnemonic_widget(Some(&activity_titles_switch));
    let activity_urls_switch = gtk::Switch::new();
    activity_urls_switch.set_halign(gtk::Align::Start);
    let activity_urls_label = gtk::Label::new(Some(CopyKey::SettingsActivityUrls.as_str()));
    activity_urls_label.set_halign(gtk::Align::Start);
    activity_urls_label.set_wrap(true);
    activity_urls_label.set_mnemonic_widget(Some(&activity_urls_switch));
    let activity_exclude_apps_entry = exclusion_text_view();
    let activity_exclude_apps_label =
        gtk::Label::new(Some(CopyKey::SettingsActivityExcludeApps.as_str()));
    activity_exclude_apps_label.set_halign(gtk::Align::Start);
    activity_exclude_apps_label.set_mnemonic_widget(Some(&activity_exclude_apps_entry));
    let activity_exclude_titles_entry = exclusion_text_view();
    let activity_exclude_titles_label =
        gtk::Label::new(Some(CopyKey::SettingsActivityExcludeTitles.as_str()));
    activity_exclude_titles_label.set_halign(gtk::Align::Start);
    activity_exclude_titles_label.set_mnemonic_widget(Some(&activity_exclude_titles_entry));
    let activity_only_switch = gtk::Switch::new();
    activity_only_switch.set_halign(gtk::Align::Start);
    let activity_only_label = gtk::Label::new(Some(CopyKey::SettingsActivityOnly.as_str()));
    activity_only_label.set_halign(gtk::Align::Start);
    activity_only_label.set_mnemonic_widget(Some(&activity_only_switch));
    let activity_only_help = gtk::Label::new(Some(CopyKey::SettingsActivityOnlyHelp.as_str()));
    activity_only_help.set_halign(gtk::Align::Start);
    activity_only_help.set_wrap(true);
    let activity_private_note =
        gtk::Label::new(Some(CopyKey::SettingsActivityPrivateWindowsNote.as_str()));
    activity_private_note.set_halign(gtk::Align::Start);
    activity_private_note.set_wrap(true);
    let activity_route_label = gtk::Label::new(Some(CopyKey::SettingsActivityRoute.as_str()));
    activity_route_label.set_halign(gtk::Align::Start);
    activity_route_label.set_wrap(true);

    let save_button = gtk::Button::with_label(CopyKey::SettingsSave.as_str());
    save_button.set_halign(gtk::Align::Start);

    let saved_banner = gtk::Label::new(Some(CopyKey::SettingsSaved.as_str()));
    saved_banner.set_halign(gtk::Align::Start);
    saved_banner.set_visible(false);

    let restart_needed_label = gtk::Label::new(Some(CopyKey::SettingsRestartNeeded.as_str()));
    restart_needed_label.set_halign(gtk::Align::Start);
    restart_needed_label.set_visible(false);

    let restart_detail_label = gtk::Label::new(Some(CopyKey::SettingsRestartDetail.as_str()));
    restart_detail_label.set_halign(gtk::Align::Start);
    restart_detail_label.set_wrap(true);
    restart_detail_label.set_visible(false);

    let restart_button = gtk::Button::with_label(CopyKey::SettingsRestart.as_str());
    restart_button.set_halign(gtk::Align::Start);
    restart_button.set_visible(false);

    let error_label = gtk::Label::new(None);
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    error_label.set_visible(false);
    let save_error_label = gtk::Label::new(None);
    save_error_label.set_halign(gtk::Align::Start);
    save_error_label.set_wrap(true);
    save_error_label.set_visible(false);

    let framerate_label = gtk::Label::new(Some(CopyKey::SettingsFrameRate.as_str()));
    framerate_label.set_halign(gtk::Align::Start);
    framerate_label.set_mnemonic_widget(Some(&framerate_entry));
    form_box.append(&framerate_label);
    form_box.append(&framerate_entry);
    form_box.append(&framerate_help);
    form_box.append(&cursor_label);
    form_box.append(&cursor_switch);
    form_box.append(&start_paused_label);
    form_box.append(&start_paused_switch);
    let segment_label = gtk::Label::new(Some(CopyKey::SettingsSegment.as_str()));
    segment_label.set_halign(gtk::Align::Start);
    segment_label.set_mnemonic_widget(Some(&segment_entry));
    cursor_label.set_mnemonic_widget(Some(&cursor_switch));
    start_paused_label.set_mnemonic_widget(Some(&start_paused_switch));
    form_box.append(&segment_label);
    form_box.append(&segment_entry);
    form_box.append(&segment_help);
    form_box.append(&activity_explain);
    form_box.append(&activity_source_label);
    form_box.append(&activity_source_switch);
    form_box.append(&activity_through_pause_label);
    form_box.append(&activity_through_pause_switch);
    form_box.append(&activity_through_pause_help);
    form_box.append(&activity_titles_label);
    form_box.append(&activity_titles_switch);
    form_box.append(&activity_urls_label);
    form_box.append(&activity_urls_switch);
    form_box.append(&activity_exclude_apps_label);
    form_box.append(&activity_exclude_apps_entry);
    form_box.append(&activity_exclude_titles_label);
    form_box.append(&activity_exclude_titles_entry);
    form_box.append(&activity_only_label);
    form_box.append(&activity_only_switch);
    form_box.append(&activity_only_help);
    form_box.append(&activity_private_note);
    form_box.append(&activity_route_label);
    form_box.append(&save_button);
    form_box.append(&saved_banner);
    form_box.append(&restart_needed_label);
    form_box.append(&error_label);

    container.append(&load_failed_label);
    container.append(&reload_button);
    container.append(&form_box);
    container.append(&restart_detail_label);
    container.append(&restart_button);
    container.append(&save_error_label);
    scrolled.set_child(Some(&container));

    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        framerate_entry.connect_changed(move |e| {
            if !rendering.get() {
                let mut state = ctrl.borrow_mut();
                state.settings_draft.framerate = parse_integer(&e.text());
                state.settings_draft.framerate_dirty = !state.document.saved_available
                    || state.settings_draft.framerate
                        != Some(state.document.settings.capture_framerate);
            }
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        segment_entry.connect_changed(move |e| {
            if !rendering.get() {
                let mut state = ctrl.borrow_mut();
                state.settings_draft.segment = parse_integer(&e.text());
                state.settings_draft.segment_dirty = !state.document.saved_available
                    || state.settings_draft.segment
                        != Some(state.document.settings.segment_interval);
            }
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        cursor_switch.connect_state_set(move |_, state| {
            if !rendering.get() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.cursor = Some(state);
                c.settings_draft.cursor_dirty =
                    !c.document.saved_available || state != c.document.settings.draw_cursor;
            }
            glib::Propagation::Proceed
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        start_paused_switch.connect_state_set(move |_, state| {
            if !rendering.get() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.start_paused = Some(state);
                c.settings_draft.start_paused_dirty =
                    !c.document.saved_available || state != c.document.settings.start_paused;
            }
            glib::Propagation::Proceed
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        activity_source_switch.connect_state_set(move |_, state| {
            if !rendering.get() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.activity_source = Some(state);
                c.settings_draft.activity_source_dirty =
                    !c.document.saved_available || state != c.document.settings.activity_source;
            }
            glib::Propagation::Proceed
        });
    }
    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        activity_through_pause_switch.connect_state_set(move |_, state| {
            if !rendering.get() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.activity_through_pause = Some(state);
                c.settings_draft.activity_through_pause_dirty = !c.document.saved_available
                    || state != c.document.settings.activity_through_pause;
            }
            glib::Propagation::Proceed
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        activity_titles_switch.connect_state_set(move |_, state| {
            if !rendering.get() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.activity_titles = Some(state);
                c.settings_draft.activity_titles_dirty =
                    !c.document.saved_available || state != c.document.settings.activity_titles;
            }
            glib::Propagation::Proceed
        });
    }
    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        activity_urls_switch.connect_state_set(move |_, state| {
            if !rendering.get() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.activity_urls = Some(state);
                c.settings_draft.activity_urls_dirty =
                    !c.document.saved_available || state != c.document.settings.activity_urls;
            }
            glib::Propagation::Proceed
        });
    }
    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        activity_only_switch.connect_state_set(move |_, state| {
            if !rendering.get() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.activity_only = Some(state);
                c.settings_draft.activity_only_dirty =
                    !c.document.saved_available || state != c.document.settings.activity_only;
            }
            glib::Propagation::Proceed
        });
    }
    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        let apps_view = activity_exclude_apps_entry.clone();
        activity_exclude_apps_entry
            .buffer()
            .connect_changed(move |_| {
                if !rendering.get() {
                    let mut c = ctrl.borrow_mut();
                    let parsed = split_exclusion_list(&text_view_text(&apps_view));
                    c.settings_draft.activity_exclude_apps_dirty = !c.document.saved_available
                        || parsed != c.document.settings.activity_exclude_apps;
                    c.settings_draft.activity_exclude_apps = Some(parsed);
                }
            });
    }
    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        let titles_view = activity_exclude_titles_entry.clone();
        activity_exclude_titles_entry
            .buffer()
            .connect_changed(move |_| {
                if !rendering.get() {
                    let mut c = ctrl.borrow_mut();
                    let parsed = split_exclusion_list(&text_view_text(&titles_view));
                    c.settings_draft.activity_exclude_titles_dirty = !c.document.saved_available
                        || parsed != c.document.settings.activity_exclude_titles;
                    c.settings_draft.activity_exclude_titles = Some(parsed);
                }
            });
    }

    let update: Rc<dyn Fn()> = {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        let save_button = save_button.clone();
        let load_failed_label = load_failed_label.clone();
        let reload_button = reload_button.clone();
        let form_box = form_box.clone();
        let framerate_entry = framerate_entry.clone();
        let cursor_switch = cursor_switch.clone();
        let start_paused_switch = start_paused_switch.clone();
        let segment_entry = segment_entry.clone();
        let activity_source_switch = activity_source_switch.clone();
        let activity_through_pause_switch = activity_through_pause_switch.clone();
        let activity_titles_switch = activity_titles_switch.clone();
        let activity_urls_switch = activity_urls_switch.clone();
        let activity_exclude_apps_entry = activity_exclude_apps_entry.clone();
        let activity_exclude_titles_entry = activity_exclude_titles_entry.clone();
        let activity_only_switch = activity_only_switch.clone();
        let activity_route_label = activity_route_label.clone();
        let framerate_entry_for_mode = framerate_entry.clone();
        let cursor_switch_for_mode = cursor_switch.clone();
        let segment_entry_for_mode = segment_entry.clone();
        let saved_banner = saved_banner.clone();
        let restart_needed_label = restart_needed_label.clone();
        let restart_detail_label = restart_detail_label.clone();
        let restart_button = restart_button.clone();
        let save_error_label = save_error_label.clone();
        Rc::new(move || {
            let (
                keys,
                framerate_dirty,
                cursor_dirty,
                start_paused_dirty,
                segment_dirty,
                activity_source_dirty,
                activity_titles_dirty,
                activity_urls_dirty,
                activity_exclude_apps_dirty,
                activity_exclude_titles_dirty,
                activity_through_pause_dirty,
                activity_only_dirty,
                saved_framerate,
                saved_cursor,
                saved_start_paused,
                saved_segment,
                saved_activity_source,
                saved_activity_titles,
                saved_activity_urls,
                saved_exclude_apps,
                saved_exclude_titles,
                saved_activity_through_pause,
                saved_activity_only,
                route_line,
                write_failed,
                write_uncertain,
                waiting_read,
                write_in_flight,
            ) = {
                let c = ctrl.borrow();
                let keys = map_settings_state(
                    c.document.saved_available,
                    &c.document.settings,
                    c.document.effective.as_ref(),
                    c.settings_draft.saved_verified && !c.settings_draft.is_dirty(),
                    c.service_owner_pid.is_some() && !c.settings_draft.is_dirty(),
                    c.settings_draft.saved_verified && c.restart_error.as_deref() == Some("failed"),
                    !c.settings_draft.saved_verified
                        && c.restart_error.as_deref() == Some("failed"),
                );
                (
                    keys,
                    c.settings_draft.framerate_dirty,
                    c.settings_draft.cursor_dirty,
                    c.settings_draft.start_paused_dirty,
                    c.settings_draft.segment_dirty,
                    c.settings_draft.activity_source_dirty,
                    c.settings_draft.activity_titles_dirty,
                    c.settings_draft.activity_urls_dirty,
                    c.settings_draft.activity_exclude_apps_dirty,
                    c.settings_draft.activity_exclude_titles_dirty,
                    c.settings_draft.activity_through_pause_dirty,
                    c.settings_draft.activity_only_dirty,
                    c.document.settings.capture_framerate,
                    c.document.settings.draw_cursor,
                    c.document.settings.start_paused,
                    c.document.settings.segment_interval,
                    c.document.settings.activity_source,
                    c.document.settings.activity_titles,
                    c.document.settings.activity_urls,
                    c.document.settings.activity_exclude_apps.clone(),
                    c.document.settings.activity_exclude_titles.clone(),
                    c.document.settings.activity_through_pause,
                    c.document.settings.activity_only,
                    activity_route_line(c.document.route_status.as_ref()),
                    c.settings_draft.write_failed,
                    c.settings_draft.write_uncertain,
                    c.settings_draft.waiting_confirming_read,
                    c.settings_draft.in_flight.is_some(),
                )
            };

            rendering.set(true);
            if keys.load_failed.is_some() {
                load_failed_label.set_visible(true);
                reload_button.set_visible(true);
                form_box.set_visible(false);
            } else {
                load_failed_label.set_visible(false);
                reload_button.set_visible(false);
                form_box.set_visible(true);

                // GTK must not replace a dirty or focused settings buffer
                if !framerate_dirty
                    && !framerate_entry
                        .state_flags()
                        .contains(gtk::StateFlags::FOCUS_WITHIN)
                    && framerate_entry.text().as_str() != format_integer(saved_framerate)
                {
                    framerate_entry.set_text(&format_integer(saved_framerate));
                }
                if !cursor_dirty
                    && !cursor_switch.has_focus()
                    && cursor_switch.is_active() != saved_cursor
                {
                    cursor_switch.set_active(saved_cursor);
                }
                if !start_paused_dirty
                    && !start_paused_switch.has_focus()
                    && start_paused_switch.is_active() != saved_start_paused
                {
                    start_paused_switch.set_active(saved_start_paused);
                }
                if !segment_dirty
                    && !segment_entry
                        .state_flags()
                        .contains(gtk::StateFlags::FOCUS_WITHIN)
                    && segment_entry.text().as_str() != format_integer(saved_segment)
                {
                    segment_entry.set_text(&format_integer(saved_segment));
                }
                if !activity_source_dirty
                    && !activity_source_switch.has_focus()
                    && activity_source_switch.is_active() != saved_activity_source
                {
                    activity_source_switch.set_active(saved_activity_source);
                }
                if !activity_through_pause_dirty
                    && !activity_through_pause_switch.has_focus()
                    && activity_through_pause_switch.is_active() != saved_activity_through_pause
                {
                    activity_through_pause_switch.set_active(saved_activity_through_pause);
                }
                if !activity_titles_dirty
                    && !activity_titles_switch.has_focus()
                    && activity_titles_switch.is_active() != saved_activity_titles
                {
                    activity_titles_switch.set_active(saved_activity_titles);
                }
                if !activity_urls_dirty
                    && !activity_urls_switch.has_focus()
                    && activity_urls_switch.is_active() != saved_activity_urls
                {
                    activity_urls_switch.set_active(saved_activity_urls);
                }
                if !activity_exclude_apps_dirty
                    && !activity_exclude_apps_entry
                        .state_flags()
                        .contains(gtk::StateFlags::FOCUS_WITHIN)
                {
                    set_text_view_text(
                        &activity_exclude_apps_entry,
                        &join_exclusion_list(&saved_exclude_apps),
                    );
                }
                if !activity_exclude_titles_dirty
                    && !activity_exclude_titles_entry
                        .state_flags()
                        .contains(gtk::StateFlags::FOCUS_WITHIN)
                {
                    set_text_view_text(
                        &activity_exclude_titles_entry,
                        &join_exclusion_list(&saved_exclude_titles),
                    );
                }
                if !activity_only_dirty
                    && !activity_only_switch.has_focus()
                    && activity_only_switch.is_active() != saved_activity_only
                {
                    activity_only_switch.set_active(saved_activity_only);
                }
                let activity_only_selected = if activity_only_dirty {
                    activity_only_switch.state()
                } else {
                    saved_activity_only
                };
                framerate_entry_for_mode.set_sensitive(!activity_only_selected);
                cursor_switch_for_mode.set_sensitive(!activity_only_selected);
                segment_entry_for_mode.set_sensitive(!activity_only_selected);
                activity_route_label.set_text(&route_line);

                saved_banner.set_visible(keys.saved_banner.is_some());
                restart_needed_label.set_visible(keys.restart_needed.is_some());
                restart_detail_label.set_visible(keys.restart_detail.is_some());
                restart_button.set_visible(keys.restart_action.is_some());

                save_button.set_sensitive(!write_in_flight && (!waiting_read || write_uncertain));
                save_button.set_label(
                    if write_in_flight || (waiting_read && !write_uncertain) {
                        CopyKey::SettingsSaving
                    } else {
                        CopyKey::SettingsSave
                    }
                    .as_str(),
                );
            }
            {
                let c = ctrl.borrow();
                let recover = c.older_daemon && c.service_owner_pid.is_some();
                restart_button
                    .set_visible(keys.restart_action.is_some() || recover || c.restart.is_some());
                restart_button.set_sensitive(
                    c.restart.is_none()
                        && c.service_owner_pid.is_some()
                        && !c.settings_draft.is_dirty(),
                );
                restart_button.set_label(
                    if c.restart.is_some() {
                        CopyKey::SettingsRestarting
                    } else {
                        CopyKey::SettingsRestart
                    }
                    .as_str(),
                );
                if let Some(key) = keys.restart_failed {
                    restart_detail_label.set_text(key.as_str());
                    restart_detail_label.set_visible(true);
                } else if c.restart_error.is_some() {
                    restart_detail_label.set_text(CopyKey::StatusStartUnavailable.as_str());
                    restart_detail_label.set_visible(true);
                } else if c.restart_verified
                    && !c.settings_draft.is_dirty()
                    && c.document.saved_available
                    && c.document.effective.as_ref() == Some(&c.document.settings)
                {
                    restart_detail_label.set_text(CopyKey::SettingsRestarted.as_str());
                    restart_detail_label.set_visible(true);
                } else {
                    restart_detail_label.set_text(CopyKey::SettingsRestartDetail.as_str());
                }
            }
            reload_button.set_visible(keys.load_failed.is_some() || write_uncertain);
            if write_failed {
                save_error_label.set_text(CopyKey::SettingsSaveIncomplete.as_str());
                save_error_label.set_visible(true);
            } else if write_uncertain || (waiting_read && keys.load_failed.is_some()) {
                save_error_label.set_text(CopyKey::SettingsSaveUnavailable.as_str());
                save_error_label.set_visible(true);
            } else {
                save_error_label.set_visible(false);
            }
            rendering.set(false);
        })
    };

    {
        let ctrl = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        reload_button.connect_clicked(move |_| {
            let ctrl = Rc::clone(&ctrl);
            let changed = Rc::clone(&changed);
            glib::MainContext::default().spawn_local(async move {
                refresh_state(&ctrl).await;
                notify(&changed);
            });
        });
    }
    {
        let ctrl = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        restart_button
            .connect_clicked(move |_| begin_restart(Rc::clone(&ctrl), Rc::clone(&changed)));
    }

    {
        let ctrl = Rc::clone(&controller);
        let framerate_entry = framerate_entry.clone();
        let segment_entry = segment_entry.clone();
        let cursor_switch = cursor_switch.clone();
        let start_paused_switch = start_paused_switch.clone();
        let error_label = error_label.clone();
        let update_fn = Rc::clone(&update);
        save_button.connect_clicked(move |_| {
            if ctrl.borrow().settings_draft.in_flight.is_some() {
                return;
            }
            let framerate_text = framerate_entry.text();
            let segment_text = segment_entry.text();
            let res = {
                let c = ctrl.borrow();
                validate_settings_changes(&c.document.settings,
                    c.settings_draft.framerate_dirty.then_some(framerate_text.as_str()),
                    c.settings_draft.segment_dirty.then_some(segment_text.as_str()))
            };
            match res {
                Ok(mut settings) => {
                    error_label.set_visible(false);
                    let writes = ctrl.borrow().settings_draft.changed_writes();
                    if writes.is_empty() {
                        return;
                    }
                    settings.draw_cursor = cursor_switch.state();
                    settings.start_paused = start_paused_switch.state();
                    let owner = ctrl.borrow().bus_owner.clone();
                    if let Some(owner) = owner {
                        let id = ctrl.borrow_mut().start_settings_save(&owner, settings);
                        update_fn();
                        let c_inner = Rc::clone(&ctrl);
                        let upd = Rc::clone(&update_fn);
                        glib::MainContext::default().spawn_local(async move {
                            match ControlClient::session().await {
                                Ok(client) => {
                                    let client = client.expect_owner(owner.clone());
                                    let mut write_res = SettingsWriteOutcome::Complete;
                                    for (field, value) in writes {
                                        match client.write_setting(field, &value).await {
                                            Ok(res) => {
                                                match solstone_linux::window_control::decode_control_str(&res) {
                                                    Ok(model) if model.saved_available && model.view != solstone_linux::window_control::SessionView::Unknown => {
                                                        if matches!(model.fault.as_deref(), Some("invalid-setting" | "invalid-config"))
                                                            || !SettingsDraft::matches_writes(&model.settings, &[(field, value)]) {
                                                            write_res = SettingsWriteOutcome::Rejected;
                                                            break;
                                                        }
                                                    }
                                                    _ => {
                                                        write_res = SettingsWriteOutcome::Unknown;
                                                        break;
                                                    }
                                                }
                                            }
                                            Err(_) => {
                                                write_res = SettingsWriteOutcome::Unknown;
                                                break;
                                            }
                                        }
                                    }
                                    c_inner
                                        .borrow_mut()
                                        .apply_settings_write_reply(id, &owner, write_res);
                                    upd();
                                }
                                Err(_) => {
                                    c_inner.borrow_mut().apply_settings_write_reply(
                                        id,
                                        &owner,
                                        SettingsWriteOutcome::Unknown,
                                    );
                                    upd();
                                }
                            }
                        });
                    }
                }
                Err(err_key) => {
                    error_label.set_text(err_key.as_str());
                    error_label.set_visible(true);
                }
            }
        });
    }

    (scrolled, update)
}

fn create_help_page(
    controller: Rc<RefCell<Controller>>,
    parent_window: &gtk::ApplicationWindow,
) -> gtk::ScrolledWindow {
    let scrolled = gtk::ScrolledWindow::new();
    let container = gtk::Box::new(gtk::Orientation::Vertical, 16);
    container.set_margin_top(24);
    container.set_margin_bottom(24);
    container.set_margin_start(24);
    container.set_margin_end(24);

    let heading = gtk::Label::new(Some(CopyKey::HelpHeading.as_str()));
    heading.set_halign(gtk::Align::Start);
    let intro = gtk::Label::new(Some(CopyKey::HelpIntro.as_str()));
    intro.set_halign(gtk::Align::Start);
    intro.set_wrap(true);

    let open_button = gtk::Button::with_label(CopyKey::HelpOpen.as_str());
    open_button.set_halign(gtk::Align::Start);
    let help_error = gtk::Label::new(None);
    help_error.set_wrap(true);
    help_error.set_halign(gtk::Align::Start);
    {
        let help_error = help_error.clone();
        open_button.connect_clicked(move |button| {
            let button = button.clone();
            let help_error = help_error.clone();
            button.set_sensitive(false);
            glib::MainContext::default().spawn_local(async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(10),
                    gio::AppInfo::launch_default_for_uri_future(
                        solstone_linux::support::HELP_URL,
                        None::<&gio::AppLaunchContext>,
                    ),
                )
                .await;
                help_error.set_text(if matches!(result, Ok(Ok(()))) {
                    ""
                } else {
                    CopyKey::HelpLinkFailed.as_str()
                });
                button.set_sensitive(true);
            });
        });
    }
    let email_button = gtk::Button::with_label(CopyKey::HelpEmail.as_str());
    email_button.set_halign(gtk::Align::Start);
    {
        let help_error = help_error.clone();
        email_button.connect_clicked(move |button| {
            let button = button.clone();
            let help_error = help_error.clone();
            button.set_sensitive(false);
            glib::MainContext::default().spawn_local(async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(10),
                    gio::AppInfo::launch_default_for_uri_future(
                        "mailto:support@solstone.app",
                        None::<&gio::AppLaunchContext>,
                    ),
                )
                .await;
                help_error.set_text(if matches!(result, Ok(Ok(()))) {
                    ""
                } else {
                    CopyKey::HelpLinkFailed.as_str()
                });
                button.set_sensitive(true);
            });
        });
    }

    let logs_title = gtk::Label::new(Some(CopyKey::LogsTitle.as_str()));
    logs_title.set_halign(gtk::Align::Start);
    let logs_intro = gtk::Label::new(Some(CopyKey::LogsIntro.as_str()));
    logs_intro.set_halign(gtk::Align::Start);
    logs_intro.set_wrap(true);

    let read_logs_btn = gtk::Button::with_label(CopyKey::LogsRead.as_str());
    read_logs_btn.set_halign(gtk::Align::Start);

    let logs_view = gtk::TextView::new();
    logs_view.set_editable(false);
    logs_view.set_wrap_mode(gtk::WrapMode::WordChar);
    let logs_scrolled = gtk::ScrolledWindow::new();
    logs_scrolled.set_min_content_height(140);
    logs_scrolled.set_child(Some(&logs_view));

    let save_logs_btn = gtk::Button::with_label(CopyKey::LogsSave.as_str());
    save_logs_btn.set_halign(gtk::Align::Start);

    let log_status = gtk::Label::new(None);
    log_status.set_halign(gtk::Align::Start);
    log_status.set_wrap(true);
    let busy = Rc::new(Cell::new(false));
    let chooser = Rc::new(RefCell::new(None::<gtk::FileChooserNative>));
    save_logs_btn.set_sensitive(false);
    let save_help = gtk::Label::new(Some(CopyKey::LogsSaveHelp.as_str()));
    save_help.set_halign(gtk::Align::Start);

    container.append(&heading);
    container.append(&intro);
    container.append(&open_button);
    container.append(&email_button);
    container.append(&help_error);
    container.append(&logs_title);
    container.append(&logs_intro);
    container.append(&read_logs_btn);
    container.append(&logs_scrolled);
    container.append(&log_status);
    container.append(&save_logs_btn);
    container.append(&save_help);
    scrolled.set_child(Some(&container));

    {
        let ctrl = Rc::clone(&controller);
        let logs_view = logs_view.clone();
        let busy = Rc::clone(&busy);
        let save_button = save_logs_btn.clone();
        let status = log_status.clone();
        read_logs_btn.connect_clicked(move |button| {
            if busy.replace(true) {
                return;
            }
            button.set_sensitive(false);
            save_button.set_sensitive(false);
            status.set_text(CopyKey::LogsReading.as_str());

            let ctrl = Rc::clone(&ctrl);
            let logs_view = logs_view.clone();
            let busy = Rc::clone(&busy);
            let read_button = button.clone();
            let save_button = save_button.clone();
            let status = status.clone();
            glib::MainContext::default().spawn_local(async move {
                let frozen = gio::spawn_blocking(collect_recent_logs).await.unwrap_or(
                    crate::logs::FrozenLog {
                        content: String::new(),
                        outcome: crate::logs::LogOutcome::Failed,
                    },
                );
                let key = match frozen.outcome {
                    crate::logs::LogOutcome::Complete => CopyKey::LogsReady,
                    crate::logs::LogOutcome::Partial => CopyKey::LogsPartial,
                    crate::logs::LogOutcome::Empty => CopyKey::LogsEmpty,
                    crate::logs::LogOutcome::Missing => CopyKey::LogsMissing,
                    crate::logs::LogOutcome::Unreadable => CopyKey::LogsUnreadable,
                    crate::logs::LogOutcome::Failed => CopyKey::LogsReadFailed,
                };
                status.set_text(key.as_str());
                if !matches!(
                    frozen.outcome,
                    crate::logs::LogOutcome::Failed
                        | crate::logs::LogOutcome::Missing
                        | crate::logs::LogOutcome::Unreadable
                ) {
                    logs_view.buffer().set_text(&frozen.content);
                    ctrl.borrow_mut().logs.frozen = Some(frozen);
                }
                save_button.set_sensitive(
                    ctrl.borrow()
                        .logs
                        .frozen
                        .as_ref()
                        .is_some_and(|preview| !preview.content.is_empty()),
                );
                read_button.set_sensitive(true);
                busy.set(false);
            });
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let parent = parent_window.clone();
        let busy = Rc::clone(&busy);
        let chooser = Rc::clone(&chooser);
        let read_button = read_logs_btn.clone();
        let status = log_status.clone();
        save_logs_btn.connect_clicked(move |button| {
            if busy.get() {
                return;
            }
            let Some(frozen) = ctrl.borrow().logs.frozen.clone() else {
                return;
            };
            busy.set(true);
            button.set_sensitive(false);
            read_button.set_sensitive(false);
            let dialog = gtk::FileChooserNative::new(
                Some(CopyKey::LogsSave.as_str()),
                Some(&parent),
                gtk::FileChooserAction::Save,
                None,
                None,
            );
            dialog.set_current_name(CopyKey::LogsFilename.as_str());
            let busy = Rc::clone(&busy);
            let read_button = read_button.clone();
            let save_button = button.clone();
            let status = status.clone();
            *chooser.borrow_mut() = Some(dialog.clone());
            let chooser = Rc::downgrade(&chooser);
            dialog.connect_response(move |dialog, response| {
                let _retained = chooser.upgrade().and_then(|slot| slot.borrow_mut().take());
                dialog.hide();
                if response != gtk::ResponseType::Accept {
                    busy.set(false);
                    read_button.set_sensitive(true);
                    save_button.set_sensitive(true);
                    return;
                }
                let Some(path) = dialog.file().and_then(|file| file.path()) else {
                    status.set_text(CopyKey::LogsSaveFailed.as_str());
                    busy.set(false);
                    read_button.set_sensitive(true);
                    save_button.set_sensitive(true);
                    return;
                };
                status.set_text(CopyKey::LogsSaving.as_str());
                let frozen = frozen.clone();
                let busy = Rc::clone(&busy);
                let read_button = read_button.clone();
                let save_button = save_button.clone();
                let status = status.clone();
                glib::MainContext::default().spawn_local(async move {
                    let result = gio::spawn_blocking(move || save_frozen_log(&frozen, &path)).await;
                    status.set_text(
                        if matches!(result, Ok(Ok(()))) {
                            CopyKey::LogsSaved
                        } else {
                            CopyKey::LogsSaveFailed
                        }
                        .as_str(),
                    );
                    busy.set(false);
                    read_button.set_sensitive(true);
                    save_button.set_sensitive(true);
                });
            });
            dialog.show();
        });
    }

    scrolled
}

#[cfg(test)]
mod tests {
    use super::{join_exclusion_list, split_exclusion_list};

    #[test]
    fn exclusion_list_keeps_a_comma_inside_one_title() {
        let items = split_exclusion_list("Budget, confidential\nOther");
        assert_eq!(
            items,
            vec!["Budget, confidential".to_string(), "Other".to_string()]
        );
        assert_eq!(join_exclusion_list(&items), "Budget, confidential\nOther");
    }
}
