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
    ServicePresenceKind, map_fault, map_intake, map_journal_health, map_obsidian,
    map_obsidian_refusal, map_open_journal_outcome, map_pairing, map_service_start,
    map_settings_state, validate_pairing_link, validate_settings_changes,
};
use crate::format::{format_date, format_integer, format_timestamp, parse_integer};
use crate::logs::{collect_recent_logs, save_frozen_log};

type Changed = Rc<RefCell<Option<Rc<dyn Fn()>>>>;
fn notify(changed: &Changed) {
    let update = changed.borrow().clone();
    if let Some(update) = update {
        update();
    }
}

const SETTINGS_PAGE_NAME: &str = "settings";

/// Brings the open window to its settings tab.
pub fn show_settings(app: &gtk::Application) {
    let Some(window) = app.windows().into_iter().next() else {
        return;
    };
    let Some(notebook) = window.child().and_downcast::<gtk::Notebook>() else {
        return;
    };
    let settings = (0..notebook.n_pages()).find(|&index| {
        notebook
            .nth_page(Some(index))
            .is_some_and(|page| page.widget_name() == SETTINGS_PAGE_NAME)
    });
    if let Some(index) = settings {
        notebook.set_current_page(Some(index));
    }
}

/// Puts a clicked pairing link on the journal tab. It never pairs: that waits for the press.
type ShowClickedLink = Rc<dyn Fn(&str)>;

thread_local! {
    static SHOW_CLICKED_LINK: RefCell<Option<ShowClickedLink>> = const { RefCell::new(None) };
}

fn show_any_clicked_link() {
    let path = solstone_linux::pending_link::default_path();
    let Some(link) = solstone_linux::pending_link::take(&path) else {
        return;
    };
    SHOW_CLICKED_LINK.with_borrow(|show| {
        if let Some(show) = show {
            show(&link);
        }
    });
}

pub fn build_ui(app: &gtk::Application) {
    if let Some(window) = app
        .active_window()
        .or_else(|| app.windows().into_iter().next())
    {
        window.present();
        show_any_clicked_link();
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
    let (journal_page, update_journal, link_entry) =
        create_journal_page(Rc::clone(&controller), Rc::clone(&changed));
    notebook.append_page(
        &journal_page,
        Some(&gtk::Label::new(Some(CopyKey::NavJournal.as_str()))),
    );

    {
        let notebook = notebook.clone();
        let journal_page = journal_page.clone();
        SHOW_CLICKED_LINK.set(Some(Rc::new(move |link: &str| {
            link_entry.set_text(link);
            notebook.set_current_page(notebook.page_num(&journal_page));
        })));
    }

    // Page 3: Settings
    let (settings_page, update_settings) =
        create_settings_page(Rc::clone(&controller), Rc::clone(&changed));
    settings_page.set_widget_name(SETTINGS_PAGE_NAME);
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

    show_any_clicked_link();

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
                let client = client.expect_owner(owner.clone());
                let reply = client.get_control_state().await;
                let obsidian = client.obsidian_state().await.ok();
                // An older running app doesn't know this call, which reads as no offer.
                let offer = match reply
                    .as_deref()
                    .map(solstone_linux::window_control::decode_control_str)
                {
                    Ok(Ok(model)) if crate::controller::offer_applies(&model) => client
                        .local_journal_offer()
                        .await
                        .ok()
                        .as_deref()
                        .and_then(solstone_linux::local_journal::decode_offer),
                    _ => None,
                };
                Some((owner, pid, reply, offer, obsidian))
            }
            Err(_) => None,
        },
        Err(_) => None,
    };
    let obsidian_doc = result.as_ref().and_then(|(_, _, _, _, obsidian)| {
        obsidian
            .as_deref()
            .and_then(solstone_linux::obsidian::decode_document)
    });
    let found_vaults = match &obsidian_doc {
        Some(doc) if doc.status.state == solstone_linux::obsidian::State::Off => {
            gio::spawn_blocking(solstone_linux::obsidian::found_vaults)
                .await
                .unwrap_or_default()
        }
        _ => Vec::new(),
    };
    let mut c = controller.borrow_mut();
    c.service_presence = presence;
    c.obsidian = obsidian_doc;
    c.obsidian_found = found_vaults;
    if let Some((owner, pid, reply, offer, _)) = result {
        c.local_journal_offer = offer;
        c.observe_service_reply(id, &owner, pid, service_pid, reply);
    } else {
        c.local_journal_offer = None;
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
    let terminal_label = gtk::Label::new(None);
    terminal_label.set_halign(gtk::Align::Start);
    terminal_label.set_wrap(true);
    terminal_label.set_visible(false);

    stats_box.append(&queue_label);
    stats_box.append(&quarantine_label);
    stats_box.append(&delivery_label);
    stats_box.append(&version_label);
    stats_box.append(&terminal_label);

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

            let terminal_status = &c.document.settings.terminal_status;
            terminal_label.set_text(&format!("terminal: {terminal_status}"));
            terminal_label.set_visible(c.document.settings.terminal && !terminal_status.is_empty());

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

/// A labelled mark for the two-mark comparison: `slot` is that side's card.
fn unknown_journal_mark_block(
    label: CopyKey,
    slot: &gtk::Widget,
    caption: Option<CopyKey>,
) -> gtk::FlowBoxChild {
    let block = gtk::Box::new(gtk::Orientation::Vertical, 6);
    block.set_halign(gtk::Align::Center);
    block.append(&gtk::Label::new(Some(label.as_str())));
    block.append(slot);
    if let Some(caption) = caption {
        block.append(&gtk::Label::new(Some(caption.as_str())));
    }
    let child = gtk::FlowBoxChild::new();
    child.set_focusable(false);
    child.set_child(Some(&block));
    child
}

/// One row: the address first, its own expand, and "pair again" only in the detail.
fn unknown_journal_row(
    row: &crate::copy_map::UnknownJournalRow,
    your_journal: &gtk::Widget,
) -> (gtk::Expander, gtk::Button) {
    let title = gtk::Label::new(Some(&row.row));
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    title.set_xalign(0.0);
    let view_details = gtk::Label::new(Some(CopyKey::UnknownJournalViewDetails.as_str()));
    view_details.add_css_class("dim-label");
    let summary = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    summary.append(&title);
    summary.append(&view_details);

    let heading = crate::mark_card::semibold_label(CopyKey::UnknownJournalHeading.as_str());
    heading.set_wrap(true);
    heading.set_xalign(0.0);
    let body = gtk::Label::new(Some(&row.body));
    body.set_wrap(true);
    body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    body.set_xalign(0.0);

    let what_answered = match row.what_answered.clone() {
        Some(mark) => crate::mark_card::create_mark_card_widget(mark),
        None => crate::mark_card::create_not_presented_mark_widget(),
    };
    let marks = gtk::FlowBox::new();
    marks.set_selection_mode(gtk::SelectionMode::None);
    marks.set_max_children_per_line(2);
    marks.set_column_spacing(12);
    marks.set_row_spacing(12);
    marks.insert(
        &unknown_journal_mark_block(
            CopyKey::UnknownJournalWhatAnswered,
            &what_answered,
            Some(row.caption),
        ),
        -1,
    );
    marks.insert(
        &unknown_journal_mark_block(CopyKey::UnknownJournalYourJournal, your_journal, None),
        -1,
    );

    let pair_again = gtk::Button::with_label(CopyKey::UnknownJournalPairAgain.as_str());
    pair_again.set_halign(gtk::Align::Start);

    let detail = gtk::Box::new(gtk::Orientation::Vertical, 12);
    detail.set_margin_top(8);
    detail.append(&heading);
    detail.append(&body);
    detail.append(&marks);
    detail.append(&pair_again);

    let expander = gtk::Expander::new(None);
    expander.set_label_widget(Some(&summary));
    expander.set_child(Some(&detail));
    (expander, pair_again)
}

fn create_journal_page(
    controller: Rc<RefCell<Controller>>,
    changed: Changed,
) -> (gtk::ScrolledWindow, Rc<dyn Fn()>, gtk::Entry) {
    let presented = Rc::new(RefCell::new(None::<crate::controller::PairingPresentation>));
    let rendered_mark = Rc::new(RefCell::new(
        None::<(
            Option<solstone_linux::window_control::MarkRenderDocument>,
            bool,
            Option<CopyKey>,
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
    let unknown_journals_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let rendered_unknown_journals = Rc::new(RefCell::new(
        None::<(
            Vec<crate::copy_map::UnknownJournalRow>,
            Option<solstone_linux::window_control::MarkRenderDocument>,
            bool,
        )>,
    ));
    let unknown_journal_rows = Rc::new(RefCell::new(Vec::<(
        Option<String>,
        gtk::Expander,
        gtk::Button,
    )>::new()));
    let mark_container = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let local_connect_button = gtk::Button::with_label(CopyKey::LocalJournalConnect.as_str());
    local_connect_button.add_css_class("suggested-action");
    local_connect_button.set_halign(gtk::Align::Center);
    local_connect_button.set_visible(false);

    let pairing_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let link_entry = gtk::Entry::new();
    let submitted_link = Rc::new(RefCell::new(None::<glib::GString>));
    link_entry.set_placeholder_text(Some(CopyKey::JournalLinkPlaceholder.as_str()));
    let pair_button = gtk::Button::with_label(CopyKey::ActionPair.as_str());
    pair_button.set_halign(gtk::Align::Start);

    let pairing_notice = gtk::Label::new(None);
    pairing_notice.set_halign(gtk::Align::Start);
    pairing_notice.set_wrap(true);
    pairing_notice.set_visible(false);
    let pairing_detail = gtk::Label::new(None);
    pairing_detail.set_halign(gtk::Align::Start);
    pairing_detail.set_wrap(true);
    pairing_detail.set_visible(false);

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
    pairing_box.append(&pairing_detail);
    pairing_box.append(&held_box);

    container.append(&status_label);
    container.append(&detail_label);
    container.append(&open_button);
    container.append(&unknown_journals_box);
    container.append(&mark_container);
    container.append(&local_connect_button);
    container.append(&pairing_box);
    scrolled.set_child(Some(&container));

    connect_open_journal(&open_button, Rc::clone(&controller), Rc::clone(&changed));

    {
        let ctrl = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        let link_entry = link_entry.clone();
        local_connect_button.connect_clicked(move |_| {
            let Some(owner) = ctrl.borrow().bus_owner.clone() else {
                return;
            };
            let Some((id, instance_id)) = ctrl.borrow_mut().start_connect_local_journal(&owner)
            else {
                return;
            };
            notify(&changed);
            let c_inner = Rc::clone(&ctrl);
            let changed = Rc::clone(&changed);
            let link_entry = link_entry.clone();
            glib::MainContext::default().spawn_local(async move {
                let res = match ControlClient::session().await {
                    Ok(client) => {
                        client
                            .expect_owner(owner.clone())
                            .connect_local_journal(&instance_id)
                            .await
                    }
                    Err(error) => Err(error),
                };
                c_inner.borrow_mut().apply_pairing_reply(id, &owner, res);
                // A link waiting in the field has nothing left to do.
                if c_inner.borrow().connected_from_offer {
                    link_entry.set_text("");
                }
                notify(&changed);
            });
        });
    }

    {
        let ctrl = Rc::clone(&controller);
        let link_entry = link_entry.clone();
        let submitted_link = Rc::clone(&submitted_link);
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
                        *submitted_link.borrow_mut() = Some(text.clone());
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

    let clicked_link_entry = link_entry.clone();

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

            // Unknown journals: rebuilt only when what they show changes, so a row
            // stays open across polls, and across another sighting coming or going.
            let unknown_rows = crate::copy_map::unknown_journal_rows(&c.document);
            let unknown_state = (
                unknown_rows.clone(),
                c.document.confirmed_mark.clone(),
                crate::copy_map::empty_mark_is_unavailable(&c.document),
            );
            if rendered_unknown_journals.borrow().as_ref() != Some(&unknown_state) {
                let mut rows = unknown_journal_rows.borrow_mut();
                let was_open: Vec<Option<String>> = rows
                    .iter()
                    .filter(|(_, expander, _)| expander.is_expanded())
                    .map(|(address, _, _)| address.clone())
                    .collect();
                while let Some(child) = unknown_journals_box.first_child() {
                    unknown_journals_box.remove(&child);
                }
                rows.clear();
                for row in &unknown_rows {
                    let your_journal = match c.document.confirmed_mark.clone() {
                        Some(mark) => crate::mark_card::create_mark_card_widget(mark),
                        None => crate::mark_card::create_empty_mark_widget(unknown_state.2),
                    };
                    let (expander, pair_again) = unknown_journal_row(row, &your_journal);
                    expander.set_expanded(was_open.contains(&row.address));
                    let link_entry = link_entry.clone();
                    pair_again.connect_clicked(move |_| {
                        link_entry.grab_focus();
                    });
                    unknown_journals_box.append(&expander);
                    rows.push((row.address.clone(), expander, pair_again));
                }
                *rendered_unknown_journals.borrow_mut() = Some(unknown_state);
            }
            unknown_journals_box.set_visible(!unknown_rows.is_empty());
            // The pairing row it goes to is hidden while a mark question is open.
            for (_, _, pair_again) in unknown_journal_rows.borrow().iter() {
                pair_again.set_visible(!crate::copy_map::attempt_pending(&c.document));
            }

            // Mark chip rendering
            let offer = c.offer_to_show();
            let mark = offer
                .map(|offer| offer.mark.clone())
                .or_else(|| crate::copy_map::mark_to_show(&c.document));
            let unavailable = crate::copy_map::empty_mark_is_unavailable(&c.document);
            let heading = if offer.is_some() {
                Some(CopyKey::LocalJournalFound)
            } else if c.connected_from_offer {
                crate::copy_map::mark_heading_after_connect(&c.document)
            } else {
                crate::copy_map::mark_heading(&c.document)
            };
            local_connect_button.set_visible(offer.is_some());
            local_connect_button.set_sensitive(c.pairing.is_none());
            local_connect_button.set_label(
                if c.pairing.as_ref().is_some_and(|command| {
                    matches!(command.payload, PairingCommand::ConnectLocalJournal(_))
                }) {
                    CopyKey::LocalJournalConnecting
                } else {
                    CopyKey::LocalJournalConnect
                }
                .as_str(),
            );
            let mark_state = Some((mark.clone(), unavailable, heading));
            if *rendered_mark.borrow() != mark_state {
                while let Some(child) = mark_container.first_child() {
                    mark_container.remove(&child);
                }
                if let (Some(mark), Some(heading)) = (mark.clone(), heading) {
                    mark_container.append(&crate::mark_card::create_headed_mark_card_widget(
                        mark, heading,
                    ));
                } else if let Some(mark) = mark.clone() {
                    mark_container.append(&crate::mark_card::create_mark_card_widget(mark));
                } else {
                    mark_container.append(&crate::mark_card::create_empty_mark_widget(unavailable));
                }
                *rendered_mark.borrow_mut() = mark_state;
            }
            mark_container.set_visible(true);

            // A later confirmation or status refresh must not erase a new link.
            if c.pairing_attempt_outcome.as_deref() == Some("confirmed")
                && let Some(used_link) = submitted_link.borrow_mut().take()
                && link_entry.text() == used_link
            {
                link_entry.set_text("");
            }

            let pairing_keys = map_pairing(&c.document, c.pairing_attempt_outcome.as_deref());
            let notice = c.browser_error.or(pairing_keys.title);
            pairing_notice.set_text(notice.map_or("", |key| key.as_str()));
            pairing_notice.set_visible(notice.is_some());
            question.set_visible(pairing_keys.secondary_title.is_some());
            question_detail.set_visible(pairing_keys.secondary_detail.is_some());
            pairing_detail.set_text(pairing_keys.detail.map_or("", |key| key.as_str()));
            pairing_detail.set_visible(pairing_keys.detail.is_some());
            // Nothing stands between the mark and the question it's asking about.
            let asking = crate::copy_map::attempt_pending(&c.document);
            link_label.set_visible(!asking);
            link_entry.set_visible(!asking);
            pair_button.set_visible(!asking);

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

    (scrolled, update, clicked_link_entry)
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
    let terminal_switch = gtk::Switch::new();
    terminal_switch.set_halign(gtk::Align::Start);
    let terminal_settings_label = gtk::Label::new(Some(CopyKey::SettingsTerminal.as_str()));
    terminal_settings_label.set_halign(gtk::Align::Start);
    terminal_settings_label.set_mnemonic_widget(Some(&terminal_switch));
    let terminal_help = gtk::Label::new(Some(CopyKey::SettingsTerminalHelp.as_str()));
    terminal_help.set_halign(gtk::Align::Start);
    terminal_help.set_wrap(true);
    let desktop_capture_switch = gtk::Switch::new();
    desktop_capture_switch.set_halign(gtk::Align::Start);
    let desktop_capture_label = gtk::Label::new(Some(CopyKey::SettingsDesktopCapture.as_str()));
    desktop_capture_label.set_halign(gtk::Align::Start);
    desktop_capture_label.set_mnemonic_widget(Some(&desktop_capture_switch));
    let desktop_capture_help = gtk::Label::new(Some(CopyKey::SettingsDesktopCaptureHelp.as_str()));
    desktop_capture_help.set_halign(gtk::Align::Start);
    desktop_capture_help.set_wrap(true);
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
    form_box.append(&terminal_settings_label);
    form_box.append(&terminal_switch);
    form_box.append(&terminal_help);
    form_box.append(&desktop_capture_label);
    form_box.append(&desktop_capture_switch);
    form_box.append(&desktop_capture_help);
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
    let (obsidian_section, update_obsidian) =
        create_obsidian_section(Rc::clone(&controller), Rc::clone(&changed));
    container.append(&obsidian_section);
    scrolled.set_child(Some(&container));

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
        terminal_switch.connect_state_set(move |_, state| {
            if !rendering.get() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.terminal = Some(state);
                c.settings_draft.terminal_dirty =
                    !c.document.saved_available || state != c.document.settings.terminal;
            }
            glib::Propagation::Proceed
        });
    }
    {
        let ctrl = Rc::clone(&controller);
        let rendering = Rc::clone(&rendering);
        desktop_capture_switch.connect_state_set(move |_, state| {
            if !rendering.get() {
                let mut c = ctrl.borrow_mut();
                c.settings_draft.desktop_capture = Some(state);
                c.settings_draft.desktop_capture_dirty =
                    !c.document.saved_available || state != c.document.settings.desktop_capture;
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
        let terminal_switch = terminal_switch.clone();
        let desktop_capture_switch = desktop_capture_switch.clone();
        let activity_route_label = activity_route_label.clone();
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
                terminal_dirty,
                desktop_capture_dirty,
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
                saved_terminal,
                saved_desktop_capture,
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
                    c.settings_draft.terminal_dirty,
                    c.settings_draft.desktop_capture_dirty,
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
                    c.document.settings.terminal,
                    c.document.settings.desktop_capture,
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
                if !terminal_dirty
                    && !terminal_switch.has_focus()
                    && terminal_switch.is_active() != saved_terminal
                {
                    terminal_switch.set_active(saved_terminal);
                }
                if !desktop_capture_dirty
                    && !desktop_capture_switch.has_focus()
                    && desktop_capture_switch.is_active() != saved_desktop_capture
                {
                    desktop_capture_switch.set_active(saved_desktop_capture);
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
            update_obsidian();
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
        let segment_entry = segment_entry.clone();
        let cursor_switch = cursor_switch.clone();
        let start_paused_switch = start_paused_switch.clone();
        let error_label = error_label.clone();
        let update_fn = Rc::clone(&update);
        save_button.connect_clicked(move |_| {
            if ctrl.borrow().settings_draft.in_flight.is_some() {
                return;
            }
            let segment_text = segment_entry.text();
            let res = {
                let c = ctrl.borrow();
                validate_settings_changes(&c.document.settings,
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

/// Ask the running app to change the Obsidian source, and take its answer.
#[derive(Clone)]
enum ObsidianOp {
    TurnOn(String),
    TurnOff,
    Continue,
    Cancel,
}

async fn obsidian_request(op: ObsidianOp) -> Option<String> {
    let client = ControlClient::session().await.ok()?;
    let result = match op {
        ObsidianOp::TurnOn(path) => client.obsidian_turn_on(&path).await,
        ObsidianOp::TurnOff => client.obsidian_turn_off().await,
        ObsidianOp::Continue => client.obsidian_continue().await,
        ObsidianOp::Cancel => client.obsidian_cancel().await,
    };
    result.ok()
}

fn run_obsidian_op(controller: Rc<RefCell<Controller>>, changed: Changed, op: ObsidianOp) {
    {
        let mut c = controller.borrow_mut();
        if c.obsidian_busy {
            return;
        }
        c.obsidian_busy = true;
        c.obsidian_refusal = None;
    }
    notify(&changed);
    glib::MainContext::default().spawn_local(async move {
        let answer = obsidian_request(op).await;
        {
            let mut c = controller.borrow_mut();
            c.obsidian_busy = false;
            if let Some(doc) = answer
                .as_deref()
                .and_then(solstone_linux::obsidian::decode_document)
            {
                c.obsidian_refusal = doc.refusal;
                c.obsidian = Some(doc);
            }
        }
        notify(&changed);
    });
}

fn format_note_count(count: u64) -> String {
    format_integer(count.min(i64::MAX as u64) as i64)
}

/// Picks and shows the Obsidian source: found vaults or a chosen folder, the refusal
/// line, the warning before the first send, progress, status and turning it off.
fn create_obsidian_section(
    controller: Rc<RefCell<Controller>>,
    changed: Changed,
) -> (gtk::Box, Rc<dyn Fn()>) {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    section.set_margin_top(12);
    section.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let heading = gtk::Label::new(Some(solstone_linux::copy::OBSIDIAN_HEADING));
    heading.set_halign(gtk::Align::Start);
    heading.add_css_class("heading");
    section.append(&heading);
    let explain = gtk::Label::new(Some(solstone_linux::copy::OBSIDIAN_EXPLAIN));
    explain.set_halign(gtk::Align::Start);
    explain.set_wrap(true);
    section.append(&explain);

    // Turning it on.
    let off_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let found_label = gtk::Label::new(Some(solstone_linux::copy::OBSIDIAN_FOUND_LABEL));
    found_label.set_halign(gtk::Align::Start);
    off_box.append(&found_label);
    let found_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    off_box.append(&found_list);
    let none_found = gtk::Label::new(Some(solstone_linux::copy::OBSIDIAN_NONE_FOUND));
    none_found.set_halign(gtk::Align::Start);
    none_found.set_wrap(true);
    off_box.append(&none_found);
    let choose_button = gtk::Button::with_label(solstone_linux::copy::OBSIDIAN_CHOOSE_FOLDER);
    choose_button.set_halign(gtk::Align::Start);
    off_box.append(&choose_button);
    let refusal_label = gtk::Label::new(None);
    refusal_label.set_halign(gtk::Align::Start);
    refusal_label.set_wrap(true);
    refusal_label.set_visible(false);
    off_box.append(&refusal_label);
    section.append(&off_box);

    // The warning before the first send.
    let warning_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let warning_heading = gtk::Label::new(Some(solstone_linux::copy::OBSIDIAN_WARNING_HEADING));
    warning_heading.set_halign(gtk::Align::Start);
    warning_box.append(&warning_heading);
    let warning_detail = gtk::Label::new(None);
    warning_detail.set_halign(gtk::Align::Start);
    warning_detail.set_wrap(true);
    warning_box.append(&warning_detail);
    let warning_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let continue_button = gtk::Button::with_label(solstone_linux::copy::OBSIDIAN_CONTINUE);
    let cancel_button = gtk::Button::with_label(solstone_linux::copy::OBSIDIAN_CANCEL);
    warning_actions.append(&continue_button);
    warning_actions.append(&cancel_button);
    warning_box.append(&warning_actions);
    section.append(&warning_box);

    // While it's on.
    let on_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let vault_label = gtk::Label::new(None);
    vault_label.set_halign(gtk::Align::Start);
    on_box.append(&vault_label);
    let state_label = gtk::Label::new(None);
    state_label.set_halign(gtk::Align::Start);
    on_box.append(&state_label);
    let detail_label = gtk::Label::new(None);
    detail_label.set_halign(gtk::Align::Start);
    detail_label.set_wrap(true);
    on_box.append(&detail_label);
    let progress_label = gtk::Label::new(None);
    progress_label.set_halign(gtk::Align::Start);
    on_box.append(&progress_label);
    let last_checked_label = gtk::Label::new(None);
    last_checked_label.set_halign(gtk::Align::Start);
    on_box.append(&last_checked_label);
    let waiting_label = gtk::Label::new(None);
    waiting_label.set_halign(gtk::Align::Start);
    on_box.append(&waiting_label);
    let not_on_device_label = gtk::Label::new(None);
    not_on_device_label.set_halign(gtk::Align::Start);
    on_box.append(&not_on_device_label);
    let too_large_label = gtk::Label::new(None);
    too_large_label.set_halign(gtk::Align::Start);
    on_box.append(&too_large_label);
    let turn_off_note = gtk::Label::new(Some(solstone_linux::copy::OBSIDIAN_TURN_OFF_NOTE));
    turn_off_note.set_halign(gtk::Align::Start);
    turn_off_note.set_wrap(true);
    on_box.append(&turn_off_note);
    let turn_off_button = gtk::Button::with_label(solstone_linux::copy::OBSIDIAN_TURN_OFF);
    turn_off_button.set_halign(gtk::Align::Start);
    on_box.append(&turn_off_button);
    section.append(&on_box);

    // The native chooser has to outlive this function, or it closes at once.
    let chooser: Rc<RefCell<Option<gtk::FileChooserNative>>> = Rc::new(RefCell::new(None));

    {
        let controller = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        let chooser = Rc::clone(&chooser);
        choose_button.connect_clicked(move |button| {
            let dialog = gtk::FileChooserNative::new(
                Some(solstone_linux::copy::OBSIDIAN_CHOOSER_TITLE),
                button.root().and_downcast::<gtk::Window>().as_ref(),
                gtk::FileChooserAction::SelectFolder,
                Some(solstone_linux::copy::OBSIDIAN_CHOOSER_ACCEPT),
                Some(solstone_linux::copy::OBSIDIAN_CANCEL),
            );
            let controller = Rc::clone(&controller);
            let changed = Rc::clone(&changed);
            let chooser_for_close = Rc::clone(&chooser);
            dialog.connect_response(move |dialog, response| {
                if response == gtk::ResponseType::Accept
                    && let Some(path) = dialog.file().and_then(|file| file.path())
                {
                    if let Some(path) = path.to_str() {
                        run_obsidian_op(
                            Rc::clone(&controller),
                            Rc::clone(&changed),
                            ObsidianOp::TurnOn(path.to_owned()),
                        );
                    } else {
                        controller.borrow_mut().obsidian_refusal =
                            Some(solstone_linux::obsidian::Refusal::UnusableName);
                        notify(&changed);
                    }
                }
                dialog.destroy();
                chooser_for_close.borrow_mut().take();
            });
            dialog.show();
            *chooser.borrow_mut() = Some(dialog);
        });
    }
    {
        let controller = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        continue_button.connect_clicked(move |_| {
            run_obsidian_op(
                Rc::clone(&controller),
                Rc::clone(&changed),
                ObsidianOp::Continue,
            );
        });
    }
    {
        let controller = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        cancel_button.connect_clicked(move |_| {
            run_obsidian_op(
                Rc::clone(&controller),
                Rc::clone(&changed),
                ObsidianOp::Cancel,
            );
        });
    }
    {
        let controller = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        turn_off_button.connect_clicked(move |_| {
            run_obsidian_op(
                Rc::clone(&controller),
                Rc::clone(&changed),
                ObsidianOp::TurnOff,
            );
        });
    }

    let shown_vaults: Rc<RefCell<Vec<solstone_linux::obsidian::FoundVault>>> =
        Rc::new(RefCell::new(Vec::new()));
    let section_ref = section.clone();
    let update = {
        let controller = Rc::clone(&controller);
        let changed = Rc::clone(&changed);
        let found_list = found_list.clone();
        Rc::new(move || {
            let (doc, found, busy, refusal) = {
                let c = controller.borrow();
                (
                    c.obsidian.clone(),
                    c.obsidian_found.clone(),
                    c.obsidian_busy,
                    c.obsidian_refusal,
                )
            };
            let Some(doc) = doc else {
                section_ref.set_visible(false);
                return;
            };
            section_ref.set_visible(true);
            let status = doc.status;
            use solstone_linux::obsidian::State;

            if *shown_vaults.borrow() != found {
                while let Some(child) = found_list.first_child() {
                    found_list.remove(&child);
                }
                for vault in &found {
                    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    let name = gtk::Label::new(Some(&vault.name));
                    name.set_halign(gtk::Align::Start);
                    name.set_hexpand(true);
                    let use_button =
                        gtk::Button::with_label(solstone_linux::copy::OBSIDIAN_USE_THIS);
                    let controller = Rc::clone(&controller);
                    let changed = Rc::clone(&changed);
                    let path = vault.path.clone();
                    use_button.connect_clicked(move |_| {
                        run_obsidian_op(
                            Rc::clone(&controller),
                            Rc::clone(&changed),
                            ObsidianOp::TurnOn(path.clone()),
                        );
                    });
                    row.append(&name);
                    row.append(&use_button);
                    found_list.append(&row);
                }
                *shown_vaults.borrow_mut() = found.clone();
            }

            off_box.set_visible(status.state == State::Off);
            found_label.set_visible(!found.is_empty());
            found_list.set_visible(!found.is_empty());
            none_found.set_visible(found.is_empty());
            choose_button.set_sensitive(!busy);
            found_list.set_sensitive(!busy);
            match refusal {
                Some(refusal) => {
                    refusal_label.set_text(map_obsidian_refusal(refusal));
                    refusal_label.set_visible(true);
                }
                None => refusal_label.set_visible(false),
            }

            warning_box.set_visible(status.state == State::Confirm);
            if let Some(warning) = &status.warning {
                let date = warning.last_synced_at.as_deref().and_then(format_date);
                warning_detail.set_text(&solstone_linux::copy::obsidian_warning_detail(
                    &warning.vault,
                    warning.device.as_deref(),
                    date.as_deref(),
                ));
            }
            continue_button.set_sensitive(!busy);
            cancel_button.set_sensitive(!busy);

            on_box.set_visible(status.state != State::Off && status.state != State::Confirm);
            let lines = map_obsidian(&status);
            state_label.set_text(lines.state.unwrap_or_default());
            state_label.set_visible(lines.state.is_some());
            detail_label.set_text(lines.detail.unwrap_or_default());
            detail_label.set_visible(lines.detail.is_some());
            if let Some(vault) = &status.vault {
                vault_label.set_text(&format!(
                    "{} {vault}",
                    solstone_linux::copy::OBSIDIAN_VAULT_LABEL
                ));
            }
            match status.first_sync {
                Some(sync) => {
                    progress_label.set_text(&solstone_linux::copy::obsidian_progress(
                        &format_note_count(sync.sent),
                        &format_note_count(sync.found),
                    ));
                    progress_label.set_visible(true);
                }
                None => progress_label.set_visible(false),
            }
            match status.last_checked_ms {
                Some(ms) => {
                    last_checked_label.set_text(&format!(
                        "{}: {}",
                        solstone_linux::copy::OBSIDIAN_LAST_CHECKED,
                        format_timestamp(ms as f64 / 1000.0)
                    ));
                    last_checked_label.set_visible(true);
                }
                None => last_checked_label.set_visible(false),
            }
            waiting_label.set_text(&solstone_linux::copy::obsidian_waiting(&format_note_count(
                status.waiting,
            )));
            not_on_device_label.set_text(&format!(
                "{}: {}",
                solstone_linux::copy::OBSIDIAN_NOT_ON_DEVICE,
                format_note_count(status.not_on_device)
            ));
            not_on_device_label.set_visible(status.not_on_device > 0);
            too_large_label.set_text(&format!(
                "{}: {}",
                solstone_linux::copy::OBSIDIAN_TOO_LARGE,
                format_note_count(status.too_large)
            ));
            too_large_label.set_visible(status.too_large > 0);
            turn_off_button.set_sensitive(!busy);
        }) as Rc<dyn Fn()>
    };

    (section, update)
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
