// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use gtk::{gio, glib, prelude::*};
use solstone_i18n::{message, text};
use solstone_linux::{
    dbus_client::ControlClient,
    import::{ImportDocument, ImportRow, RecentReceipt, RemoveConfirmation},
    shell_model,
    source_intake::IntakeFile,
};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::deck::{MarkKind, apply_mark_kind, fact_row, labeled_button, mark_widget};

const RECENT_DISPLAY_LIMIT: usize = 5;

#[derive(Clone)]
pub struct ImportPage {
    pub widget: gtk::Box,
    choose: gtk::Button,
    more_box: gtk::Box,
    addition: gtk::Label,
    intake_refused: gtk::Box,
    notice: gtk::Box,
    error: gtk::Label,
    review_card: gtk::Box,
    review_rows: gtk::Box,
    device_section: gtk::Box,
    queue_hold: gtk::Label,
    rows: gtk::Box,
    recent_section: gtk::Box,
    recent_rows: gtk::Box,
    recent_empty: gtk::Label,
    all_history: gtk::Button,
    facts_box: gtk::Box,
    so_far_row: gtk::Box,
    so_far: gtk::Label,
    last_row: gtk::Box,
    last: gtk::Label,
    store_down: Rc<Cell<bool>>,
    last_offline: Rc<Cell<bool>>,
    model: Rc<RefCell<Option<(String, bool, u64)>>>,
    chooser: Rc<RefCell<Option<gtk::FileChooserNative>>>,
}

fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_halign(gtk::Align::Start);
    label.add_css_class("sol-quiet");
    label
}

fn name_label(text: &str) -> gtk::Label {
    let label = label(text);
    label.add_css_class("sol-ink");
    label.add_css_class("sol-import-name");
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label
}

fn section(text: &str) -> gtk::Label {
    let label = label(text);
    label.add_css_class("sol-section");
    label.add_css_class("sol-faint");
    label.add_css_class("sol-rule");
    label
}

fn ok(answer: Result<String, solstone_linux::dbus_client::ControlClientError>) -> bool {
    answer
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|v| v["ok"] == true)
}

pub fn file_paths(files: impl IntoIterator<Item = gio::File>) -> Vec<String> {
    files
        .into_iter()
        .map(|file| {
            file.path()
                .unwrap_or_else(|| PathBuf::from(file.uri().as_str()))
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

pub fn drop_paths(value: &glib::Value) -> Option<Vec<String>> {
    if let Ok(list) = value.get::<gtk::gdk::FileList>() {
        return Some(file_paths(list.files()));
    }
    value.get::<gio::File>().ok().map(|file| file_paths([file]))
}

fn minute_bucket() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 60)
        .unwrap_or(0)
}

fn is_review_row(row: &ImportRow) -> bool {
    row.review_expired || matches!(row.state.as_str(), "ready to review" | "review expired")
}

fn is_expired_review(row: &ImportRow) -> bool {
    row.review_expired || row.state == "review expired"
}

fn review_detail(row: &ImportRow) -> &'static str {
    if is_expired_review(row) {
        text!("linux.import.reviewexpireddetail")
    } else if row.review_kind.as_deref() == Some("body_confirmation_required") {
        text!("linux.import.reviewdetailhour")
    } else {
        text!("linux.import.reviewdetail")
    }
}

fn review_target_path(row: &ImportRow) -> String {
    match row.import_id.as_deref() {
        Some(id) if solstone_linux::import::valid_import_id(id) => format!("/app/import/{id}"),
        _ => "/app/import/".to_owned(),
    }
}

fn recent_target_path(receipt: &RecentReceipt) -> Option<String> {
    receipt
        .import_id
        .as_deref()
        .filter(|id| solstone_linux::import::valid_import_id(id))
        .map(|id| format!("/app/import/{id}"))
}

fn device_row_priority(row: &ImportRow) -> u8 {
    match row.state.as_str() {
        "needs attention" => 0,
        "sending" => 1,
        _ => 2,
    }
}

fn sort_device_rows<'a>(rows: &'a [ImportRow]) -> Vec<&'a ImportRow> {
    let mut device: Vec<&'a ImportRow> = rows.iter().filter(|row| !is_review_row(row)).collect();
    device.sort_by_key(|row| device_row_priority(row));
    device
}

fn queue_hold_line(document: &ImportDocument, offline: bool) -> Option<&str> {
    if let Some(hold) = document.queue_hold.as_deref() {
        Some(hold)
    } else if offline {
        Some(text!("linux.import.waitingreconnect"))
    } else {
        None
    }
}

fn device_row_detail(row: &ImportRow) -> Option<String> {
    match row.state.as_str() {
        "sending" => Some(text!("linux.import.sendingellipsis").to_owned()),
        "waiting" => Some(text!("linux.import.waitingdetail").to_owned()),
        "needs attention" => row
            .reason
            .as_deref()
            .map(|r| solstone_linux::import::display_import_status(r).to_owned()),
        _ => row
            .reason
            .as_deref()
            .map(|r| solstone_linux::import::display_import_status(r).to_owned()),
    }
}

fn recent_subtitle(when: &str) -> String {
    if when.is_empty() {
        text!("linux.import.delivered").to_owned()
    } else {
        message!("linux.import.inyourjournalrel", "when" => when)
    }
}

pub(crate) fn import_facts(file: &IntakeFile) -> (Option<String>, Option<String>) {
    let total = shell_model::imports_so_far(file);
    let (started, last_raw) = match file {
        IntakeFile::Rows(rows) => (
            rows.imports.started.as_deref(),
            rows.imports.last.as_deref(),
        ),
        IntakeFile::Missing | IntakeFile::Refused => (None, None),
    };
    let last = shell_model::source_last_now(file, last_raw);
    (
        shell_model::page_total_now(&total, started),
        shell_model::last_text(&last).map(str::to_owned),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ImportPaneLayout {
    show_more_imports: bool,
    show_review_card: bool,
    show_device_section: bool,
    show_recent_section: bool,
    show_all_history: bool,
    review_count: usize,
    device_count: usize,
}

fn pane_layout(document: &ImportDocument) -> ImportPaneLayout {
    if document.store_error {
        return ImportPaneLayout {
            show_more_imports: false,
            show_review_card: false,
            show_device_section: false,
            show_recent_section: false,
            show_all_history: false,
            review_count: 0,
            device_count: 0,
        };
    }
    let review_count = document.rows.iter().filter(|r| is_review_row(r)).count();
    let device_count = document.rows.len().saturating_sub(review_count);
    ImportPaneLayout {
        show_more_imports: document.paired_confirmed,
        show_review_card: review_count > 0,
        show_device_section: device_count > 0,
        show_recent_section: true,
        show_all_history: document.paired_confirmed,
        review_count,
        device_count,
    }
}

impl ImportPage {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 10);

        let choose = gtk::Button::with_label(text!("linux.window.choosefiles"));
        choose.add_css_class("sol-action");
        choose.set_halign(gtk::Align::Start);

        let more_box = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let more = gtk::Button::with_label(text!("linux.import.moreimports"));
        more.add_css_class("sol-action");
        more.set_halign(gtk::Align::Start);
        let more_sub = label(text!("linux.import.moreimportssub"));
        more_sub.add_css_class("sol-faint");
        more_sub.add_css_class("sol-more-sub");
        more_box.append(&more);
        more_box.append(&more_sub);
        more_box.set_visible(false);

        let addition = label("");
        addition.add_css_class("sol-faint");
        addition.set_visible(false);
        let intake_refused = gtk::Box::new(gtk::Orientation::Vertical, 4);
        intake_refused.set_visible(false);
        let error = label("");
        error.add_css_class("sol-attn");
        error.set_visible(false);
        let notice = gtk::Box::new(gtk::Orientation::Vertical, 6);
        notice.append(&label(text!(
            "linux.window.somethingyouwereaddingdidntfinish"
        )));
        let dismiss = gtk::Button::with_label(text!("linux.window.ok"));
        dismiss.add_css_class("sol-action");
        dismiss.set_halign(gtk::Align::Start);
        notice.append(&dismiss);
        notice.set_visible(false);

        let review_card = gtk::Box::new(gtk::Orientation::Vertical, 10);
        review_card.add_css_class("sol-review-card");
        let review_heading = label(text!("linux.import.waitingforyourreview"));
        review_heading.add_css_class("sol-section");
        review_heading.add_css_class("sol-review-word");
        review_card.append(&review_heading);
        let review_rows = gtk::Box::new(gtk::Orientation::Vertical, 10);
        review_card.append(&review_rows);
        review_card.set_visible(false);

        let device_section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let device_heading = section(text!("linux.window.onthisdevice"));
        device_section.append(&device_heading);
        let queue_hold = label("");
        queue_hold.add_css_class("sol-faint");
        queue_hold.set_visible(false);
        device_section.append(&queue_hold);
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 10);
        device_section.append(&rows);
        device_section.set_visible(false);

        let recent_section = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let recent_heading = section(text!("linux.window.recent"));
        recent_section.append(&recent_heading);
        let recent_rows = gtk::Box::new(gtk::Orientation::Vertical, 2);
        recent_section.append(&recent_rows);
        let recent_empty = label(text!("linux.window.nothingyet"));
        recent_empty.add_css_class("sol-faint");
        recent_section.append(&recent_empty);
        let all_history = gtk::Button::with_label(text!("linux.import.allhistory"));
        all_history.add_css_class("sol-back");
        all_history.set_has_frame(false);
        all_history.set_halign(gtk::Align::Start);
        all_history.set_margin_top(4);
        all_history.set_visible(false);
        recent_section.append(&all_history);

        let facts_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        facts_box.set_margin_top(8);
        let (so_far_row, so_far) = fact_row(text!("linux.window.sofar"));
        let (last_row, last) = fact_row(text!("linux.window.lasttakenin"));
        so_far_row.set_visible(false);
        last_row.set_visible(false);
        facts_box.append(&so_far_row);
        facts_box.append(&last_row);
        facts_box.set_visible(false);

        widget.append(&choose);
        widget.append(&more_box);
        widget.append(&addition);
        widget.append(&intake_refused);
        widget.append(&error);
        widget.append(&notice);
        widget.append(&review_card);
        widget.append(&device_section);
        widget.append(&recent_section);
        widget.append(&facts_box);

        let page = Self {
            widget,
            choose,
            more_box,
            addition,
            intake_refused,
            notice,
            error,
            review_card,
            review_rows,
            device_section,
            queue_hold,
            rows,
            recent_section,
            recent_rows,
            recent_empty,
            all_history,
            facts_box,
            so_far_row,
            so_far,
            last_row,
            last,
            store_down: Rc::default(),
            last_offline: Rc::default(),
            model: Rc::default(),
            chooser: Rc::default(),
        };

        let opener = page.clone();
        more.connect_clicked(move |_| opener.open_journal_path("/app/import/".to_owned()));

        let history_opener = page.clone();
        page.all_history
            .connect_clicked(move |_| history_opener.open_journal_path("/app/import/".to_owned()));

        let picked = page.clone();
        page.choose.connect_clicked(move |button| {
            if picked.chooser.borrow().is_some() {
                return;
            }
            let Some(window) = button.root().and_downcast::<gtk::Window>() else {
                return;
            };
            let chooser = gtk::FileChooserNative::new(
                Some(text!("linux.window.choosefiles")),
                Some(&window),
                gtk::FileChooserAction::Open,
                Some(text!("linux.window.choosefiles")),
                Some(text!("linux.window.cancel")),
            );
            chooser.set_select_multiple(true);
            let page = picked.clone();
            let chooser_for_close = picked.chooser.clone();
            chooser.connect_response(move |chooser, response| {
                if response == gtk::ResponseType::Accept {
                    let files = chooser.files();
                    let paths = file_paths(
                        (0..files.n_items())
                            .filter_map(|i| files.item(i).and_downcast::<gio::File>()),
                    );
                    page.add(paths);
                }
                chooser.destroy();
                chooser_for_close.borrow_mut().take();
            });
            chooser.show();
            *picked.chooser.borrow_mut() = Some(chooser);
        });

        let page_dismiss = page.clone();
        dismiss.connect_clicked(move |_| {
            page_dismiss.clear_step();
            let page = page_dismiss.clone();
            glib::MainContext::default().spawn_local(async move {
                let success = match ControlClient::session().await {
                    Ok(client) => {
                        let success = client.import_dismiss_interrupted().await.is_ok();
                        page.reload(&client).await;
                        success
                    }
                    Err(_) => false,
                };
                if !success {
                    page.fail(solstone_linux::import::step_error());
                } else {
                    page.clear_step();
                }
            });
        });

        page
    }

    pub fn focus_entry(&self) -> bool {
        if self.choose.is_visible() && self.choose.is_sensitive() {
            self.choose.grab_focus()
        } else {
            false
        }
    }

    fn open_journal_path(&self, path: String) {
        self.clear_step();
        let page = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let opened = match ControlClient::session().await {
                Ok(client) => client
                    .open_journal_at_path(&path)
                    .await
                    .ok()
                    .is_some_and(|reply| {
                        crate::copy_map::map_open_journal_outcome(&reply).is_none()
                    }),
                Err(_) => false,
            };
            if !opened {
                page.fail(solstone_linux::import::step_error());
            }
        });
    }

    fn fail(&self, text: &str) {
        self.error.set_text(if self.store_down.get() {
            solstone_linux::import::store_error()
        } else {
            text
        });
        self.error.set_visible(true);
    }

    fn clear_step(&self) {
        if !self.store_down.get() {
            self.error.set_text("");
            self.error.set_visible(false);
        }
    }

    async fn reload(&self, client: &ControlClient) {
        if let Ok(raw) = client.import_state().await
            && let Ok(document) = serde_json::from_str::<ImportDocument>(&raw)
        {
            self.refresh_document(&document, self.last_offline.get());
        }
    }

    pub fn add(&self, paths: Vec<String>) {
        if self.store_down.get() {
            self.fail(solstone_linux::import::store_error());
            return;
        }
        if paths.is_empty() {
            return;
        }
        self.choose.set_sensitive(false);
        self.clear_step();
        let page = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let success = match ControlClient::session().await {
                Ok(client) => {
                    let success = ok(client.import_files(paths).await);
                    page.reload(&client).await;
                    success
                }
                Err(_) => false,
            };
            if !success {
                page.fail(solstone_linux::import::step_error());
            }
            if success {
                page.clear_step();
            }
            page.choose.set_sensitive(!page.store_down.get());
        });
    }

    pub fn update_intake(&self, intake: &IntakeFile) {
        let (so_far, last) = import_facts(intake);
        if let Some(text) = &so_far {
            self.so_far.set_text(text);
            self.so_far_row.set_visible(true);
        } else {
            self.so_far_row.set_visible(false);
        }
        if let Some(text) = &last {
            self.last.set_text(text);
            self.last_row.set_visible(true);
        } else {
            self.last_row.set_visible(false);
        }
        self.facts_box
            .set_visible(!self.store_down.get() && (so_far.is_some() || last.is_some()));
    }

    pub fn refresh(&self, document: &ImportDocument, offline: bool, intake: &IntakeFile) {
        self.refresh_document(document, offline);
        self.update_intake(intake);
    }

    fn refresh_document(&self, document: &ImportDocument, offline: bool) {
        self.last_offline.set(offline);
        let encoded = serde_json::to_string(document).expect("import document serializes");
        let cache_key = (encoded, offline, minute_bucket());
        if self.model.borrow().as_ref() == Some(&cache_key) {
            return;
        }
        *self.model.borrow_mut() = Some(cache_key);

        let layout = pane_layout(document);
        self.choose
            .set_sensitive(!document.adding && !document.store_error);
        self.more_box.set_visible(layout.show_more_imports);

        self.addition
            .set_text(document.addition.as_deref().unwrap_or(""));
        self.addition
            .set_visible(document.addition.is_some() && !document.store_error);
        clear_box(&self.intake_refused);
        for refusal in &document.intake_refused {
            let line = label(&refusal.line());
            line.add_css_class("sol-faint");
            self.intake_refused.append(&line);
        }
        self.intake_refused.set_visible(
            document.addition.is_some()
                && !document.store_error
                && !document.intake_refused.is_empty(),
        );

        let was_down = self.store_down.replace(document.store_error);
        if was_down && !document.store_error {
            self.clear_step();
        }
        self.notice
            .set_visible(document.interrupted && !document.store_error);

        if document.store_error {
            self.fail(solstone_linux::import::store_error());
        }

        // 1. waiting for your review
        self.review_card.set_visible(layout.show_review_card);
        clear_box(&self.review_rows);
        if layout.show_review_card {
            let review_items: Vec<&ImportRow> =
                document.rows.iter().filter(|r| is_review_row(r)).collect();
            for (index, row) in review_items.into_iter().enumerate() {
                if index > 0 {
                    self.review_rows
                        .append(&gtk::Separator::new(gtk::Orientation::Horizontal));
                }
                self.review_rows.append(&self.build_review_row(row));
            }
        }

        // 2. on this device (hidden when empty)
        self.device_section.set_visible(layout.show_device_section);
        clear_box(&self.rows);
        if layout.show_device_section {
            if let Some(hold) = queue_hold_line(document, offline) {
                self.queue_hold.set_text(hold);
                self.queue_hold.set_visible(true);
            } else {
                self.queue_hold.set_text("");
                self.queue_hold.set_visible(false);
            }
            for row in sort_device_rows(&document.rows) {
                self.rows.append(&self.build_device_row(row));
            }
        } else {
            self.queue_hold.set_visible(false);
        }

        // 3. recent (24h, up to 5)
        self.recent_section.set_visible(layout.show_recent_section);
        clear_box(&self.recent_rows);
        if layout.show_recent_section {
            let shown = document.recent_receipts.len().min(RECENT_DISPLAY_LIMIT);
            self.recent_empty.set_visible(shown == 0);
            self.recent_rows.set_visible(shown > 0);
            for receipt in document.recent_receipts.iter().take(RECENT_DISPLAY_LIMIT) {
                self.recent_rows.append(&self.build_recent_row(receipt));
            }
            self.all_history.set_visible(layout.show_all_history);
        }
    }

    fn build_review_row(&self, row: &ImportRow) -> gtk::Box {
        let expired = is_expired_review(row);
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
        box_.append(&name_label(&row.name));

        let status = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let mark = mark_widget();
        apply_mark_kind(
            &mark,
            if expired {
                MarkKind::Expired
            } else {
                MarkKind::Review
            },
        );
        let state_label = label(if expired {
            text!("linux.import.expired")
        } else {
            text!("linux.import.readytoreview")
        });
        state_label.add_css_class("sol-state");
        state_label.add_css_class("sol-review-word");
        status.append(&mark);
        status.append(&state_label);
        box_.append(&status);

        let detail = label(review_detail(row));
        detail.add_css_class("sol-faint");
        box_.append(&detail);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_margin_top(4);
        if expired {
            let send_again = labeled_button(
                text!("linux.import.sendagain"),
                &shell_model::import_send_again_a11y(&row.name),
            );
            send_again.add_css_class("sol-action");
            send_again.add_css_class("sol-primary");
            send_again.set_sensitive(row.can_retry);
            self.wire_retry(&send_again, row.id.clone());
            actions.append(&send_again);
        } else {
            let review = labeled_button(
                text!("linux.import.reviewinjournal"),
                &shell_model::import_review_a11y(&row.name),
            );
            review.add_css_class("sol-action");
            review.add_css_class("sol-primary");
            let page = self.clone();
            let target = review_target_path(row);
            review.connect_clicked(move |_| page.open_journal_path(target.clone()));
            actions.append(&review);
        }

        let remove = labeled_button(
            text!("linux.window.remove"),
            &shell_model::import_remove_a11y(&row.name),
        );
        remove.add_css_class("sol-action");
        remove.set_sensitive(row.can_remove);
        let page = self.clone();
        let row_clone = row.clone();
        remove.connect_clicked(move |_| page.confirm_remove(row_clone.clone()));
        actions.append(&remove);

        box_.append(&actions);
        box_
    }

    fn build_device_row(&self, row: &ImportRow) -> gtk::Box {
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
        box_.add_css_class("sol-import-row");
        box_.append(&name_label(&row.name));

        let status = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let mark = mark_widget();
        let (mark_kind, state_text, word_class) = match row.state.as_str() {
            "needs attention" => (
                MarkKind::Diamond,
                text!("linux.import.attention"),
                "sol-attn",
            ),
            "sending" => (MarkKind::Dot, text!("linux.import.sending"), "sol-ink"),
            _ => (MarkKind::Ring, text!("linux.import.waiting"), "sol-faint"),
        };
        apply_mark_kind(&mark, mark_kind);
        let state_label = label(state_text);
        state_label.add_css_class("sol-state");
        state_label.add_css_class(word_class);
        status.append(&mark);
        status.append(&state_label);
        box_.append(&status);

        if let Some(detail_text) = device_row_detail(row) {
            let detail = label(&detail_text);
            detail.add_css_class("sol-faint");
            box_.append(&detail);
        }

        if row.state != "sending" && (row.can_remove || row.can_retry) {
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            actions.set_margin_top(2);
            if row.can_retry {
                let retry = labeled_button(
                    text!("linux.window.trynow"),
                    &shell_model::import_try_now_a11y(&row.name),
                );
                retry.add_css_class("sol-action");
                self.wire_retry(&retry, row.id.clone());
                actions.append(&retry);
            }
            let remove = labeled_button(
                text!("linux.window.remove"),
                &shell_model::import_remove_a11y(&row.name),
            );
            remove.add_css_class("sol-action");
            remove.set_sensitive(row.can_remove);
            let page = self.clone();
            let row_clone = row.clone();
            remove.connect_clicked(move |_| page.confirm_remove(row_clone.clone()));
            actions.append(&remove);
            box_.append(&actions);
        }

        box_
    }

    fn build_recent_row(&self, receipt: &RecentReceipt) -> gtk::Widget {
        let rel = shell_model::format_relative_millis(receipt.delivered_at_epoch_ms);
        let sub_text = recent_subtitle(&rel);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let mark = mark_widget();
        apply_mark_kind(&mark, MarkKind::Good);
        mark.set_valign(gtk::Align::Start);
        mark.set_margin_top(4);
        content.append(&mark);

        let lines = gtk::Box::new(gtk::Orientation::Vertical, 1);
        lines.set_hexpand(true);
        lines.append(&name_label(&receipt.filename));
        let sub = label(&sub_text);
        sub.add_css_class("sol-faint");
        lines.append(&sub);
        content.append(&lines);

        if let Some(target) = recent_target_path(receipt) {
            let chevron = gtk::Label::new(Some("›"));
            chevron.add_css_class("sol-faint");
            chevron.set_valign(gtk::Align::Center);
            content.append(&chevron);

            let a11y = shell_model::import_recent_row_a11y(
                &receipt.filename,
                if rel.is_empty() {
                    text!("linux.about.now")
                } else {
                    &rel
                },
            );
            let button = labeled_button("", &a11y);
            button.add_css_class("sol-recent-row");
            button.set_has_frame(false);
            button.set_halign(gtk::Align::Fill);
            button.set_child(Some(&content));
            let page = self.clone();
            button.connect_clicked(move |_| page.open_journal_path(target.clone()));
            button.upcast()
        } else {
            content.add_css_class("sol-recent-row-static");
            content.upcast()
        }
    }

    fn wire_retry(&self, button: &gtk::Button, id: String) {
        let page = self.clone();
        button.connect_clicked(move |button| {
            page.clear_step();
            button.set_sensitive(false);
            let page = page.clone();
            let id = id.clone();
            let button = button.clone();
            glib::MainContext::default().spawn_local(async move {
                let success = match ControlClient::session().await {
                    Ok(client) => {
                        let success = ok(client.import_retry(&id).await);
                        page.reload(&client).await;
                        success
                    }
                    Err(_) => false,
                };
                if !success {
                    page.fail(solstone_linux::import::step_error());
                } else {
                    page.clear_step();
                }
                button.set_sensitive(true);
            });
        });
    }

    fn confirm_remove(&self, row: ImportRow) {
        self.clear_step();
        let page = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let Ok(client) = ControlClient::session().await else {
                page.fail(solstone_linux::import::step_error());
                return;
            };
            let confirmation = client
                .import_begin_remove(&row.id)
                .await
                .ok()
                .and_then(|raw| serde_json::from_str::<Option<RemoveConfirmation>>(&raw).ok());
            match confirmation {
                Some(Some(confirmation)) => page.present_remove(Rc::new(client), confirmation),
                Some(None) => page.reload(&client).await,
                None => page.fail(solstone_linux::import::step_error()),
            }
        });
    }

    fn present_remove(&self, client: Rc<ControlClient>, confirmation: RemoveConfirmation) {
        let Some(window) = self.widget.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&window)
            .modal(true)
            .text(message!("linux.window.remove.vf022f0fa", "value1" => confirmation.row.name))
            .secondary_text(warning(&confirmation.row))
            .build();
        dialog.add_button(text!("linux.window.cancel"), gtk::ResponseType::Cancel);
        dialog.add_button(text!("linux.window.remove"), gtk::ResponseType::Accept);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        let error = label(solstone_linux::import::remove_error());
        error.add_css_class("sol-attn");
        error.set_visible(false);
        dialog.content_area().append(&error);
        let page = self.clone();
        dialog.connect_response(move |dialog, response| {
            error.set_visible(false);
            dialog.set_response_sensitive(gtk::ResponseType::Accept, false);
            let dialog = dialog.clone();
            let client = client.clone();
            let confirmation = confirmation.clone();
            let page = page.clone();
            let error = error.clone();
            glib::MainContext::default().spawn_local(async move {
                if response != gtk::ResponseType::Accept {
                    let _ = client
                        .import_cancel_remove(&confirmation.row.id, &confirmation.token)
                        .await;
                    dialog.close();
                    page.reload(&client).await;
                    return;
                }
                let result = client
                    .import_remove(
                        &confirmation.row.id,
                        &confirmation.token,
                        confirmation.row.may_have_saved,
                    )
                    .await;
                if result.ok().as_deref() == Some("removed") {
                    dialog.close();
                    page.reload(&client).await;
                    return;
                }
                error.set_visible(true);
                dialog.set_response_sensitive(gtk::ResponseType::Accept, true);
            });
        });
        dialog.present();
    }
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn warning(row: &ImportRow) -> &'static str {
    if row.missing {
        text!("linux.window.thecopyisalreadygonesothis")
    } else if is_review_row(row) {
        text!("linux.import.removereadytoreview")
    } else if row.may_have_saved {
        text!("linux.window.thisremovesthecopyonthisdevice")
    } else {
        text!("linux.window.thisremovesthecopyonthisdevice.v558a0c4f")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row(id: &str, name: &str, state: &str) -> ImportRow {
        ImportRow {
            id: id.into(),
            name: name.into(),
            state: state.into(),
            can_remove: state != "sending",
            ..ImportRow::default()
        }
    }

    #[test]
    fn file_list_keeps_unsupported_and_nonlocal_inputs_visible_to_intake() {
        let paths = file_paths([
            gio::File::for_path("/tmp/note.txt"),
            gio::File::for_path("/tmp/movie.mp4"),
            gio::File::for_uri("https://example.invalid/file"),
        ]);
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[1], "/tmp/movie.mp4");
        assert_eq!(paths[2], "https://example.invalid/file");
        assert_eq!(
            drop_paths(&gio::File::for_path("/tmp/movie.mp4").to_value()).unwrap(),
            vec!["/tmp/movie.mp4"]
        );
        assert!(drop_paths(&42i32.to_value()).is_none());
    }

    #[test]
    fn pane_layout_hides_more_imports_when_unconfirmed_and_hides_empty_device_section() {
        let unpaired = ImportDocument {
            paired_confirmed: false,
            ..ImportDocument::default()
        };
        let layout = pane_layout(&unpaired);
        assert!(!layout.show_more_imports);
        assert!(!layout.show_all_history);
        assert!(!layout.show_review_card);
        assert!(!layout.show_device_section);
        assert!(layout.show_recent_section);

        let paired_with_review_only = ImportDocument {
            paired_confirmed: true,
            ready_to_review: 1,
            rows: vec![ImportRow {
                import_id: Some("20261121_181500".into()),
                review_kind: Some("body_confirmation_required".into()),
                ..sample_row("1", "export.zip", "ready to review")
            }],
            ..ImportDocument::default()
        };
        let layout = pane_layout(&paired_with_review_only);
        assert!(layout.show_more_imports);
        assert!(layout.show_all_history);
        assert!(layout.show_review_card);
        assert!(!layout.show_device_section);
        assert_eq!(layout.review_count, 1);
        assert_eq!(layout.device_count, 0);
    }

    #[test]
    fn device_rows_sort_actionable_first_and_exclude_review_rows() {
        let rows = vec![
            sample_row("w", "later.zip", "waiting"),
            sample_row("r", "export.zip", "ready to review"),
            sample_row("s", "notes.md", "sending"),
            sample_row("a", "broken.bin", "needs attention"),
            sample_row("e", "old.zip", "review expired"),
        ];
        let sorted = sort_device_rows(&rows);
        let ids: Vec<&str> = sorted.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "s", "w"]);
    }

    #[test]
    fn review_details_targets_and_remove_warnings_match_locked_copy() {
        let body_confirm = ImportRow {
            import_id: Some("20261121_181500".into()),
            review_kind: Some("body_confirmation_required".into()),
            ..sample_row("1", "export.zip", "ready to review")
        };
        assert_eq!(
            review_detail(&body_confirm),
            "saved in your journal. review within an hour before your journal removes the waiting copy."
        );
        assert_eq!(
            review_target_path(&body_confirm),
            "/app/import/20261121_181500"
        );
        assert_eq!(
            warning(&body_confirm),
            "this removes the copy on this device. the waiting copy in your journal stays there until you discard it or it expires."
        );

        let generic_confirm = ImportRow {
            import_id: Some("20261121_181501".into()),
            review_kind: Some("confirmation_required".into()),
            ..sample_row("2", "notes.enex", "ready to review")
        };
        assert_eq!(
            review_detail(&generic_confirm),
            "saved in your journal. preview and confirm to start."
        );

        let expired = ImportRow {
            import_id: Some("20261121_181500".into()),
            review_kind: Some("body_confirmation_required".into()),
            review_expired: true,
            ..sample_row("3", "export.zip", "review expired")
        };
        assert_eq!(
            review_detail(&expired),
            "the waiting copy in your journal expired. this device still holds the file."
        );
        assert_eq!(
            warning(&expired),
            "this removes the copy on this device. the waiting copy in your journal stays there until you discard it or it expires."
        );

        let local_only = sample_row("4", "photo.jpg", "waiting");
        assert_eq!(
            warning(&local_only),
            "this removes the copy on this device, so it won't reach your journal."
        );

        let missing = ImportRow {
            missing: true,
            ..sample_row("5", "gone.jpg", "needs attention")
        };
        assert_eq!(
            warning(&missing),
            "the copy is already gone, so this only clears it from here."
        );
    }

    #[test]
    fn recent_target_path_validates_import_id_and_queue_hold_handles_offline() {
        let good = RecentReceipt {
            filename: "export.zip".into(),
            import_id: Some("20261121_181500".into()),
            instance_id: "inst".into(),
            delivered_at_epoch_ms: 1_700_000_000_000,
        };
        assert_eq!(
            recent_target_path(&good).as_deref(),
            Some("/app/import/20261121_181500")
        );
        let bad = RecentReceipt {
            import_id: Some("../settings".into()),
            ..good.clone()
        };
        assert_eq!(recent_target_path(&bad), None);

        let doc = ImportDocument::default();
        assert_eq!(queue_hold_line(&doc, false), None);
        assert_eq!(
            queue_hold_line(&doc, true),
            Some("waiting for your journal to reconnect.")
        );
        let held_doc = ImportDocument {
            queue_hold: Some("imports wait until you confirm your journal's mark.".into()),
            ..ImportDocument::default()
        };
        assert_eq!(
            queue_hold_line(&held_doc, true),
            Some("imports wait until you confirm your journal's mark.")
        );
    }
}
