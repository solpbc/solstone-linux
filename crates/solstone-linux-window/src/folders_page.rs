// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The folders page under its verdict: configured folder list, add-folder flow
//! with overlap & refusal checks, per-folder detail & setup (six kind switches,
//! optional facet picker, history/backfill chooser with non-money estimate,
//! pause/resume, on/off, repoint, and inline remove confirmation).

use solstone_i18n::text;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use solstone_linux::copy::folders as copy;
use solstone_linux::copy::screenshots as shots_copy;
use solstone_linux::folders::{
    FacetOption, FacetsAvailability, FolderActionRequest, FolderAttention, FolderKinds, FolderMode,
    FolderSummary, FoldersDocument, PathCheckResult,
};
use solstone_linux::screenshots::{Span, group};
use solstone_linux::shell_model;
use solstone_linux::source_intake::IntakeFile;

use crate::deck::{fact_row, mark_widget, note_label, paint_mark, paint_word, section_label};

const SPANS: [Span; 5] = [
    Span::Last30Days,
    Span::Last6Months,
    Span::LastYear,
    Span::All,
    Span::Today,
];

fn action(text: &str) -> gtk::Button {
    let button = gtk::Button::with_label(text);
    button.add_css_class("sol-action");
    button.set_halign(gtk::Align::Start);
    button
}

fn ink(label: gtk::Label) -> gtk::Label {
    label.remove_css_class("sol-faint");
    label.add_css_class("sol-ink");
    label
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn format_local_date(ms: i64) -> String {
    if let Ok(dt) = glib::DateTime::from_unix_local(ms / 1000)
        && let Ok(text) = dt.format("%b %-d, %Y")
    {
        return text.to_string();
    }
    String::new()
}

pub struct FoldersSection {
    pub root: gtk::Box,

    // List view
    list_box: gtk::Box,
    empty_breath: gtk::Label,
    list_rows_box: gtk::Box,
    add_button: gtk::Button,
    check_card: gtk::Box,
    check_heading: gtk::Label,
    check_body: gtk::Label,
    check_actions: gtk::Box,
    check_primary: gtk::Button,
    check_secondary: gtk::Button,
    list_so_far_row: gtk::Box,
    list_so_far: gtk::Label,
    list_last_row: gtk::Box,
    list_last: gtk::Label,

    // Detail / Setup view
    detail_box: gtk::Box,
    back_button: gtk::Button,
    detail_name: gtk::Label,
    detail_mark: gtk::DrawingArea,
    detail_word: gtk::Label,
    detail_switch: gtk::Switch,
    pause_row: gtk::Box,
    pause_button: gtk::Button,
    resume_button: gtk::Button,

    banner_box: gtk::Box,
    banner_text: gtk::Label,
    banner_primary: gtk::Button,
    banner_secondary: gtk::Button,
    progress_label: gtk::Label,
    cant_read_box: gtk::Box,
    cant_read_label: gtk::Label,
    cant_read_retry: gtk::Button,
    fallback_label: gtk::Label,

    // Kind checkboxes
    kind_checks: [gtk::CheckButton; 6],
    kinds_error: gtk::Label,
    kinds_note: gtk::Label,

    // Facet picker
    facet_dropdown: gtk::DropDown,
    facet_help: gtk::Label,

    // History / Backfill chooser
    chooser_box: gtk::Box,
    span_radios: Vec<gtk::CheckButton>,
    preview_status: gtk::Label,
    preview_split: gtk::Label,
    preview_est_heading: gtk::Label,
    preview_est_values: gtk::Label,
    preview_est_gloss: gtk::Label,
    preview_scope: gtk::Label,
    preview_retry: gtk::Button,
    chooser_note: gtk::Label,
    confirm_button: gtk::Button,
    backfill_cancel: gtk::Button,

    bring_older_button: gtk::Button,

    // Facts & metadata
    path_value: gtk::Label,
    change_folder_button: gtk::Button,
    overlap_fact_label: gtk::Label,
    detail_so_far_row: gtk::Box,
    detail_so_far: gtk::Label,
    detail_last_row: gtk::Box,
    detail_last: gtk::Label,
    detail_checked_row: gtk::Box,
    detail_checked: gtk::Label,
    detail_waiting_row: gtk::Box,
    detail_waiting: gtk::Label,
    detail_other_note: gtk::Label,

    // Remove flow
    remove_button: gtk::Button,
    remove_card: gtk::Box,
    remove_heading: gtk::Label,
    remove_body: gtk::Label,
    remove_confirm_btn: gtk::Button,
    remove_cancel_btn: gtk::Button,

    // UI state
    selected_id: RefCell<Option<String>>,
    pending_check: RefCell<Option<PathCheckResult>>,
    span: Cell<Span>,
    backfill_open: Cell<bool>,
    remove_confirm_open: Cell<bool>,
    busy: Cell<bool>,
    painting: Cell<bool>,
    shown: Cell<bool>,
    facets_requested: Cell<bool>,
    doc: RefCell<Option<FoldersDocument>>,
    list_signature: RefCell<Vec<String>>,
    facets_signature: RefCell<Vec<String>>,
    picker: RefCell<Option<gtk::FileChooserNative>>,
}

impl FoldersSection {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_visible(false);

        // --- List view ---
        let list_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let empty_breath = note_label(Some(copy::intro()));
        list_box.append(&empty_breath);

        let list_rows_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        list_box.append(&list_rows_box);

        let add_button = action(copy::choose_folder());
        list_box.append(&add_button);

        let check_card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        check_card.add_css_class("sol-remote-card");
        check_card.set_visible(false);
        let check_heading = ink(note_label(None));
        check_heading.add_css_class("sol-state");
        let check_body = note_label(None);
        let check_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let check_primary = action(copy::overlap_choose());
        let check_secondary = action(copy::overlap_override());
        check_actions.append(&check_primary);
        check_actions.append(&check_secondary);
        check_card.append(&check_heading);
        check_card.append(&check_body);
        check_card.append(&check_actions);
        list_box.append(&check_card);

        let (list_so_far_row, list_so_far) = fact_row(text!("linux.window.sofar"));
        let (list_last_row, list_last) = fact_row(text!("linux.window.lasttakenin"));
        list_box.append(&list_so_far_row);
        list_box.append(&list_last_row);

        let list_trust = note_label(Some(copy::f17()));
        list_trust.add_css_class("sol-quiet");
        list_box.append(&list_trust);

        root.append(&list_box);

        // --- Detail / Setup view ---
        let detail_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        detail_box.set_visible(false);

        let back_button = gtk::Button::with_label(&format!("\u{2039} {}", copy::all_folders()));
        back_button.add_css_class("sol-back");
        back_button.set_has_frame(false);
        back_button.set_halign(gtk::Align::Start);
        detail_box.append(&back_button);

        let detail_head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let detail_name = gtk::Label::new(None);
        detail_name.add_css_class("sol-name");
        detail_name.add_css_class("sol-display");
        detail_name.add_css_class("sol-ink");
        detail_name.set_halign(gtk::Align::Start);
        detail_name.set_hexpand(true);
        detail_name.set_wrap(true);
        detail_name.set_xalign(0.0);
        detail_head.append(&detail_name);
        detail_box.append(&detail_head);

        let detail_verdict = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let detail_mark = mark_widget();
        let detail_word = gtk::Label::new(None);
        detail_word.add_css_class("sol-state");
        detail_word.set_halign(gtk::Align::Start);
        let verdict_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        verdict_spacer.set_hexpand(true);
        let detail_switch = gtk::Switch::new();
        detail_switch.add_css_class("sol-switch");
        detail_switch.set_valign(gtk::Align::Center);
        detail_verdict.append(&detail_mark);
        detail_verdict.append(&detail_word);
        detail_verdict.append(&verdict_spacer);
        detail_verdict.append(&detail_switch);
        detail_box.append(&detail_verdict);

        let pause_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let pause_button = action(solstone_linux::copy::CopyKey::ActionPause.as_str());
        let resume_button = action(solstone_linux::copy::CopyKey::ActionResume.as_str());
        pause_row.append(&pause_button);
        pause_row.append(&resume_button);
        pause_row.set_visible(false);
        detail_box.append(&pause_row);

        let banner_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        banner_box.set_visible(false);
        let banner_text = ink(note_label(None));
        let banner_btns = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let banner_primary = action(shots_copy::choose_folder());
        let banner_secondary = action(shots_copy::s12_action());
        banner_btns.append(&banner_primary);
        banner_btns.append(&banner_secondary);
        banner_box.append(&banner_text);
        banner_box.append(&banner_btns);
        detail_box.append(&banner_box);

        let progress_label = ink(note_label(None));
        progress_label.set_visible(false);
        detail_box.append(&progress_label);

        let cant_read_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        cant_read_box.set_visible(false);
        let cant_read_label = ink(note_label(None));
        let cant_read_retry = action(shots_copy::s12_action());
        cant_read_box.append(&cant_read_label);
        cant_read_box.append(&cant_read_retry);
        detail_box.append(&cant_read_box);

        let fallback_label = note_label(None);
        fallback_label.add_css_class("sol-quiet");
        fallback_label.set_visible(false);
        detail_box.append(&fallback_label);

        // Kinds section
        let kinds_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let kinds_heading = section_label(copy::what_comes_in());
        kinds_heading.add_css_class("sol-rule");
        kinds_box.append(&kinds_heading);
        let kind_defs = [
            copy::kind_notes(),
            copy::kind_documents(),
            copy::kind_transcripts(),
            copy::kind_screenshots(),
            copy::kind_photos(),
            copy::kind_other(),
        ];
        let mut kind_checks_vec = Vec::with_capacity(6);
        for (title, desc) in kind_defs {
            let item = gtk::Box::new(gtk::Orientation::Vertical, 1);
            let check = gtk::CheckButton::with_label(title);
            check.add_css_class("sol-ink");
            check.add_css_class("sol-choice");
            check.set_active(true);
            let sub = note_label(Some(desc));
            sub.add_css_class("sol-quiet");
            sub.set_margin_start(26);
            item.append(&check);
            item.append(&sub);
            kinds_box.append(&item);
            kind_checks_vec.push(check);
        }
        let kind_checks: [gtk::CheckButton; 6] = kind_checks_vec
            .try_into()
            .expect("six folder kinds always build");
        let kinds_error = note_label(Some(copy::choose_at_least_one()));
        kinds_error.remove_css_class("sol-faint");
        kinds_error.add_css_class("sol-attn");
        kinds_error.set_visible(false);
        kinds_box.append(&kinds_error);
        let kinds_note = note_label(Some(copy::kinds_going_forward()));
        kinds_note.add_css_class("sol-quiet");
        kinds_note.set_visible(false);
        kinds_box.append(&kinds_note);
        detail_box.append(&kinds_box);

        // Facet section
        let facet_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let facet_heading = section_label(copy::facet_heading());
        facet_heading.add_css_class("sol-rule");
        facet_box.append(&facet_heading);
        let initial_facets = gtk::StringList::new(&[copy::facet_none()]);
        let facet_dropdown = gtk::DropDown::new(Some(initial_facets), gtk::Expression::NONE);
        facet_dropdown.set_halign(gtk::Align::Fill);
        let facet_help = note_label(Some(copy::facet_help_none()));
        facet_help.add_css_class("sol-quiet");
        facet_box.append(&facet_dropdown);
        facet_box.append(&facet_help);
        detail_box.append(&facet_box);

        // History / Backfill chooser
        let chooser_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        chooser_box.set_visible(false);
        let chooser_heading = section_label(shots_copy::s7());
        chooser_heading.add_css_class("sol-rule");
        chooser_box.append(&chooser_heading);
        let mut span_radios: Vec<gtk::CheckButton> = Vec::new();
        for text in shots_copy::s8() {
            let radio = gtk::CheckButton::with_label(text);
            radio.add_css_class("sol-ink");
            radio.add_css_class("sol-choice");
            if let Some(first) = span_radios.first() {
                radio.set_group(Some(first));
            }
            chooser_box.append(&radio);
            span_radios.push(radio);
        }
        span_radios[0].set_active(true);

        let preview_card = gtk::Box::new(gtk::Orientation::Vertical, 5);
        preview_card.add_css_class("sol-remote-card");
        let preview_head = section_label(copy::preview_heading());
        let preview_status = ink(note_label(Some(copy::checking_folder())));
        let preview_split = note_label(None);
        let preview_est_heading = section_label(copy::est_heading());
        preview_est_heading.set_margin_top(4);
        let preview_est_values = ink(note_label(None));
        let preview_est_gloss = note_label(Some(copy::est_gloss()));
        preview_est_gloss.add_css_class("sol-quiet");
        let preview_scope = note_label(Some(copy::scope_note()));
        preview_scope.add_css_class("sol-quiet");
        let preview_retry = action(shots_copy::s12_action());
        preview_retry.set_visible(false);
        preview_card.append(&preview_head);
        preview_card.append(&preview_status);
        preview_card.append(&preview_split);
        preview_card.append(&preview_est_heading);
        preview_card.append(&preview_est_values);
        preview_card.append(&preview_est_gloss);
        preview_card.append(&preview_scope);
        preview_card.append(&preview_retry);
        chooser_box.append(&preview_card);

        let chooser_note = note_label(Some(copy::nothing_until_confirm()));
        chooser_note.add_css_class("sol-quiet");
        chooser_box.append(&chooser_note);

        let confirm_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let confirm_button = action(copy::checking_folder());
        let backfill_cancel = action(solstone_linux::copy::obsidian_cancel());
        backfill_cancel.set_visible(false);
        confirm_row.append(&confirm_button);
        confirm_row.append(&backfill_cancel);
        chooser_box.append(&confirm_row);
        detail_box.append(&chooser_box);

        let bring_older_button = action(copy::bring_in_older());
        bring_older_button.set_visible(false);
        detail_box.append(&bring_older_button);

        // Facts
        let facts_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let facts_rule = section_label(shots_copy::s22());
        facts_rule.add_css_class("sol-rule");
        facts_box.append(&facts_rule);
        let (path_row, path_value) = fact_row(shots_copy::s22());
        let change_folder_button = action(shots_copy::choose_folder());
        let overlap_fact_label = note_label(None);
        overlap_fact_label.add_css_class("sol-quiet");
        overlap_fact_label.set_visible(false);
        let (detail_so_far_row, detail_so_far) = fact_row(text!("linux.window.sofar"));
        let (detail_last_row, detail_last) = fact_row(text!("linux.window.lasttakenin"));
        let (detail_checked_row, detail_checked) = fact_row(shots_copy::s18());
        let (detail_waiting_row, detail_waiting) = fact_row(shots_copy::s19());
        let detail_trust = note_label(Some(copy::f17()));
        detail_trust.add_css_class("sol-quiet");
        let detail_other_note = note_label(Some(copy::f18()));
        detail_other_note.add_css_class("sol-quiet");
        facts_box.append(&path_row);
        facts_box.append(&change_folder_button);
        facts_box.append(&overlap_fact_label);
        facts_box.append(&detail_so_far_row);
        facts_box.append(&detail_last_row);
        facts_box.append(&detail_checked_row);
        facts_box.append(&detail_waiting_row);
        facts_box.append(&detail_trust);
        facts_box.append(&detail_other_note);
        detail_box.append(&facts_box);

        // Remove folder section
        let remove_button = action(copy::f22());
        remove_button.set_margin_top(8);
        detail_box.append(&remove_button);

        let remove_card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        remove_card.add_css_class("sol-remote-card");
        remove_card.set_visible(false);
        let remove_heading = ink(note_label(None));
        remove_heading.add_css_class("sol-state");
        let remove_body = note_label(None);
        let remove_btns = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let remove_confirm_btn = action(copy::f22());
        let remove_cancel_btn = action(solstone_linux::copy::obsidian_cancel());
        remove_btns.append(&remove_confirm_btn);
        remove_btns.append(&remove_cancel_btn);
        remove_card.append(&remove_heading);
        remove_card.append(&remove_body);
        remove_card.append(&remove_btns);
        detail_box.append(&remove_card);

        root.append(&detail_box);

        let section = Rc::new(Self {
            root,
            list_box,
            empty_breath,
            list_rows_box,
            add_button,
            check_card,
            check_heading,
            check_body,
            check_actions,
            check_primary,
            check_secondary,
            list_so_far_row,
            list_so_far,
            list_last_row,
            list_last,
            detail_box,
            back_button,
            detail_name,
            detail_mark,
            detail_word,
            detail_switch,
            pause_row,
            pause_button,
            resume_button,
            banner_box,
            banner_text,
            banner_primary,
            banner_secondary,
            progress_label,
            cant_read_box,
            cant_read_label,
            cant_read_retry,
            fallback_label,
            kind_checks,
            kinds_error,
            kinds_note,
            facet_dropdown,
            facet_help,
            chooser_box,
            span_radios,
            preview_status,
            preview_split,
            preview_est_heading,
            preview_est_values,
            preview_est_gloss,
            preview_scope,
            preview_retry,
            chooser_note,
            confirm_button,
            backfill_cancel,
            bring_older_button,
            path_value,
            change_folder_button,
            overlap_fact_label,
            detail_so_far_row,
            detail_so_far,
            detail_last_row,
            detail_last,
            detail_checked_row,
            detail_checked,
            detail_waiting_row,
            detail_waiting,
            detail_other_note,
            remove_button,
            remove_card,
            remove_heading,
            remove_body,
            remove_confirm_btn,
            remove_cancel_btn,
            selected_id: RefCell::new(None),
            pending_check: RefCell::new(None),
            span: Cell::new(Span::Last30Days),
            backfill_open: Cell::new(false),
            remove_confirm_open: Cell::new(false),
            busy: Cell::new(false),
            painting: Cell::new(false),
            shown: Cell::new(false),
            facets_requested: Cell::new(false),
            doc: RefCell::new(None),
            list_signature: RefCell::new(Vec::new()),
            facets_signature: RefCell::new(Vec::new()),
            picker: RefCell::new(None),
        });

        section.wire_signals();
        section
    }

    fn wire_signals(self: &Rc<Self>) {
        // Add / choose folder
        {
            let weak = Rc::downgrade(self);
            self.add_button.connect_clicked(move |btn| {
                if let Some(s) = weak.upgrade() {
                    s.pick_folder(btn, None);
                }
            });
        }

        // Check-card primary / secondary actions
        {
            let weak = Rc::downgrade(self);
            self.check_primary.connect_clicked(move |btn| {
                let Some(s) = weak.upgrade() else {
                    return;
                };
                let check = s.pending_check.borrow().clone();
                match check {
                    Some(PathCheckResult::AlreadyAdded { folder_id, .. }) => {
                        *s.pending_check.borrow_mut() = None;
                        s.open_detail(folder_id);
                    }
                    Some(PathCheckResult::Overlaps { .. })
                    | Some(PathCheckResult::Refused { .. }) => {
                        *s.pending_check.borrow_mut() = None;
                        s.repaint_from_cached();
                        s.pick_folder(btn, None);
                    }
                    _ => {}
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.check_secondary.connect_clicked(move |_| {
                let Some(s) = weak.upgrade() else {
                    return;
                };
                let check = s.pending_check.borrow().clone();
                if let Some(PathCheckResult::Overlaps { path, target, .. }) = check {
                    *s.pending_check.borrow_mut() = None;
                    s.run_action(
                        FolderActionRequest::BeginSetup {
                            path,
                            overlap: Some(target),
                        },
                        true,
                    );
                }
            });
        }

        // Back button (detail -> list)
        {
            let weak = Rc::downgrade(self);
            self.back_button.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade() {
                    *s.selected_id.borrow_mut() = None;
                    s.backfill_open.set(false);
                    s.remove_confirm_open.set(false);
                    s.repaint_from_cached();
                }
            });
        }

        // Per-folder on/off switch
        {
            let weak = Rc::downgrade(self);
            self.detail_switch.connect_state_set(move |_, on| {
                let Some(s) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if s.painting.get() {
                    return glib::Propagation::Proceed;
                }
                if let Some(id) = s.selected_id.borrow().clone() {
                    let req = if on {
                        FolderActionRequest::TurnOn { id }
                    } else {
                        FolderActionRequest::TurnOff { id }
                    };
                    s.run_action(req, false);
                }
                glib::Propagation::Proceed
            });
        }

        // Pause / Resume buttons
        {
            let weak = Rc::downgrade(self);
            self.pause_button.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade()
                    && let Some(id) = s.selected_id.borrow().clone()
                {
                    s.run_action(FolderActionRequest::Pause { id }, false);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.resume_button.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade()
                    && let Some(id) = s.selected_id.borrow().clone()
                {
                    s.run_action(FolderActionRequest::Resume { id }, false);
                }
            });
        }

        // Banner primary (choose folder / repoint) and secondary (try again)
        {
            let weak = Rc::downgrade(self);
            self.banner_primary.connect_clicked(move |btn| {
                if let Some(s) = weak.upgrade()
                    && let Some(id) = s.selected_id.borrow().clone()
                {
                    s.pick_folder(btn, Some(id));
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.banner_secondary.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade()
                    && let Some(id) = s.selected_id.borrow().clone()
                {
                    s.run_action(FolderActionRequest::Retry { id }, false);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.cant_read_retry.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade()
                    && let Some(id) = s.selected_id.borrow().clone()
                {
                    s.run_action(FolderActionRequest::Retry { id }, false);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.change_folder_button.connect_clicked(move |btn| {
                if let Some(s) = weak.upgrade()
                    && let Some(id) = s.selected_id.borrow().clone()
                {
                    s.pick_folder(btn, Some(id));
                }
            });
        }

        // Kind checkboxes
        for check in &self.kind_checks {
            let weak = Rc::downgrade(self);
            check.connect_toggled(move |_| {
                let Some(s) = weak.upgrade() else {
                    return;
                };
                if s.painting.get() {
                    return;
                }
                let Some(id) = s.selected_id.borrow().clone() else {
                    return;
                };
                let kinds = s.read_kinds();
                s.kinds_error.set_visible(!kinds.any_selected());
                if !kinds.any_selected() {
                    s.confirm_button.set_sensitive(false);
                }
                s.run_action(FolderActionRequest::SetKinds { id, kinds }, false);
            });
        }

        // Facet dropdown
        {
            let weak = Rc::downgrade(self);
            self.facet_dropdown.connect_selected_notify(move |dd| {
                let Some(s) = weak.upgrade() else {
                    return;
                };
                if s.painting.get() {
                    return;
                }
                let Some(id) = s.selected_id.borrow().clone() else {
                    return;
                };
                let idx = dd.selected() as usize;
                let facet: Option<FacetOption> = if idx == 0 {
                    None
                } else {
                    s.doc
                        .borrow()
                        .as_ref()
                        .and_then(|d| d.facets.get(idx - 1).cloned())
                };
                s.run_action(FolderActionRequest::SetFacet { id, facet }, false);
            });
        }

        // History span radios
        for (radio, span) in self.span_radios.iter().zip(SPANS) {
            let weak = Rc::downgrade(self);
            radio.connect_toggled(move |btn| {
                let Some(s) = weak.upgrade() else {
                    return;
                };
                if btn.is_active() && !s.painting.get() {
                    s.span.set(span);
                    if let Some(id) = s.selected_id.borrow().clone() {
                        s.run_action(FolderActionRequest::Preview { id, span }, false);
                    }
                }
            });
        }

        // Preview retry
        {
            let weak = Rc::downgrade(self);
            self.preview_retry.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade()
                    && let Some(id) = s.selected_id.borrow().clone()
                {
                    s.run_action(
                        FolderActionRequest::Preview {
                            id,
                            span: s.span.get(),
                        },
                        false,
                    );
                }
            });
        }

        // Confirm setup / backfill
        {
            let weak = Rc::downgrade(self);
            self.confirm_button.connect_clicked(move |_| {
                let Some(s) = weak.upgrade() else {
                    return;
                };
                let Some(id) = s.selected_id.borrow().clone() else {
                    return;
                };
                if !s.read_kinds().any_selected() {
                    return;
                }
                let span = s.span.get();
                s.backfill_open.set(false);
                s.run_action(FolderActionRequest::Confirm { id, span }, false);
            });
        }

        // Bring in older files & cancel
        {
            let weak = Rc::downgrade(self);
            self.bring_older_button.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade()
                    && let Some(id) = s.selected_id.borrow().clone()
                {
                    s.backfill_open.set(true);
                    s.repaint_from_cached();
                    s.run_action(
                        FolderActionRequest::Preview {
                            id,
                            span: s.span.get(),
                        },
                        false,
                    );
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.backfill_cancel.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade() {
                    s.backfill_open.set(false);
                    s.repaint_from_cached();
                }
            });
        }

        // Remove folder + inline confirmation
        {
            let weak = Rc::downgrade(self);
            self.remove_button.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade() {
                    s.remove_confirm_open.set(true);
                    s.repaint_from_cached();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.remove_cancel_btn.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade() {
                    s.remove_confirm_open.set(false);
                    s.repaint_from_cached();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.remove_confirm_btn.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade()
                    && let Some(id) = s.selected_id.borrow().clone()
                {
                    s.remove_confirm_open.set(false);
                    *s.selected_id.borrow_mut() = None;
                    s.backfill_open.set(false);
                    s.run_action(FolderActionRequest::Remove { id }, false);
                }
            });
        }
    }

    pub fn set_shown(self: &Rc<Self>, shown: bool) {
        self.shown.set(shown);
        if shown {
            if !self.facets_requested.replace(true) {
                self.run_action(FolderActionRequest::RefreshFacets, false);
            }
            self.maybe_request_preview();
        }
    }

    fn open_detail(self: &Rc<Self>, id: String) {
        *self.selected_id.borrow_mut() = Some(id);
        self.backfill_open.set(false);
        self.remove_confirm_open.set(false);
        self.repaint_from_cached();
        self.maybe_request_preview();
    }

    fn read_kinds(&self) -> FolderKinds {
        FolderKinds {
            notes: self.kind_checks[0].is_active(),
            documents: self.kind_checks[1].is_active(),
            transcripts: self.kind_checks[2].is_active(),
            screenshots: self.kind_checks[3].is_active(),
            photos: self.kind_checks[4].is_active(),
            other: self.kind_checks[5].is_active(),
        }
    }

    fn pick_folder(self: &Rc<Self>, button: &gtk::Button, repoint_id: Option<String>) {
        let picker = gtk::FileChooserNative::new(
            Some(copy::choose_folder()),
            button.root().and_downcast::<gtk::Window>().as_ref(),
            gtk::FileChooserAction::SelectFolder,
            Some(solstone_linux::copy::obsidian_chooser_accept()),
            Some(solstone_linux::copy::obsidian_cancel()),
        );
        let weak = Rc::downgrade(self);
        picker.connect_response(move |picker, response| {
            if let Some(s) = weak.upgrade() {
                if response == gtk::ResponseType::Accept
                    && let Some(path) = picker.file().and_then(|file| file.path())
                    && let Some(path_str) = path.to_str()
                {
                    let chosen = path_str.to_owned();
                    if let Some(id) = repoint_id.clone() {
                        s.run_action(FolderActionRequest::Repoint { id, path: chosen }, false);
                    } else {
                        s.check_new_folder(chosen);
                    }
                }
                s.picker.borrow_mut().take();
            }
            picker.destroy();
        });
        picker.show();
        *self.picker.borrow_mut() = Some(picker);
    }

    fn check_new_folder(self: &Rc<Self>, path: String) {
        if self.busy.get() {
            return;
        }
        self.busy.set(true);
        self.paint_busy();
        let section = Rc::clone(self);
        glib::MainContext::default().spawn_local(async move {
            let res = check_path_dbus(&path).await;
            section.busy.set(false);
            match res {
                Some(PathCheckResult::Ready { path, .. }) => {
                    *section.pending_check.borrow_mut() = None;
                    section.run_action(
                        FolderActionRequest::BeginSetup {
                            path,
                            overlap: None,
                        },
                        true,
                    );
                }
                Some(other) => {
                    *section.pending_check.borrow_mut() = Some(other);
                    section.repaint_from_cached();
                }
                None => {
                    section.paint_busy();
                }
            }
        });
    }

    fn run_action(self: &Rc<Self>, req: FolderActionRequest, select_newest: bool) {
        if self.busy.get() {
            return;
        }
        let prev_ids: Vec<String> = self
            .doc
            .borrow()
            .as_ref()
            .map(|d| d.folders.iter().map(|f| f.id.clone()).collect())
            .unwrap_or_default();
        self.busy.set(true);
        self.paint_busy();
        let section = Rc::clone(self);
        glib::MainContext::default().spawn_local(async move {
            let raw = send_action_dbus(&req).await;
            section.busy.set(false);
            if let Some(doc) = raw
                .as_deref()
                .and_then(solstone_linux::folders::decode_document)
            {
                if select_newest
                    && let Some(added) = doc
                        .folders
                        .iter()
                        .find(|f| !prev_ids.contains(&f.id))
                        .or_else(|| doc.folders.last())
                {
                    *section.selected_id.borrow_mut() = Some(added.id.clone());
                    section.backfill_open.set(false);
                    section.remove_confirm_open.set(false);
                }
                crate::deck::take_folders(doc);
                section.maybe_request_preview();
            } else {
                section.paint_busy();
            }
        });
    }

    fn maybe_request_preview(self: &Rc<Self>) {
        if !self.shown.get() || self.busy.get() {
            return;
        }
        let Some(id) = self.selected_id.borrow().clone() else {
            return;
        };
        let needs = self.doc.borrow().as_ref().is_some_and(|doc| {
            doc.folders.iter().find(|f| f.id == id).is_some_and(|f| {
                let chooser_shown = f.mode == FolderMode::SettingUp || self.backfill_open.get();
                chooser_shown
                    && f.kinds.any_selected()
                    && f.preview
                        .as_ref()
                        .is_none_or(|p| !p.failed && p.span != Some(self.span.get()))
            })
        });
        if needs {
            self.run_action(
                FolderActionRequest::Preview {
                    id,
                    span: self.span.get(),
                },
                false,
            );
        }
    }

    fn paint_busy(&self) {
        let busy = self.busy.get();
        self.add_button.set_sensitive(!busy);
        self.pause_button.set_sensitive(!busy);
        self.resume_button.set_sensitive(!busy);
        self.change_folder_button.set_sensitive(!busy);
        self.bring_older_button.set_sensitive(!busy);
        self.remove_confirm_btn.set_sensitive(!busy);
        for radio in &self.span_radios {
            radio.set_sensitive(!busy);
        }
        if busy {
            self.confirm_button.set_sensitive(false);
        }
    }

    fn repaint_from_cached(self: &Rc<Self>) {
        crate::deck::refresh_now();
    }

    pub fn update(self: &Rc<Self>, doc: Option<&FoldersDocument>, intake: &IntakeFile) {
        *self.doc.borrow_mut() = doc.cloned();
        let Some(doc) = doc else {
            self.root.set_visible(false);
            return;
        };
        self.root.set_visible(true);

        // If selected folder disappeared, fall back to list.
        let active_folder: Option<FolderSummary> = {
            let sel = self.selected_id.borrow().clone();
            sel.and_then(|id| doc.folders.iter().find(|f| f.id == id).cloned())
        };
        if active_folder.is_none() && self.selected_id.borrow().is_some() {
            *self.selected_id.borrow_mut() = None;
            self.backfill_open.set(false);
            self.remove_confirm_open.set(false);
        }

        if let Some(folder) = active_folder {
            self.list_box.set_visible(false);
            self.detail_box.set_visible(true);
            self.paint_detail(doc, &folder);
        } else {
            self.detail_box.set_visible(false);
            self.list_box.set_visible(true);
            self.paint_list(doc, intake);
        }
        self.paint_busy();
    }

    fn paint_list(self: &Rc<Self>, doc: &FoldersDocument, intake: &IntakeFile) {
        let empty = doc.folders.is_empty();
        self.empty_breath.set_visible(empty);
        self.add_button.set_label(if empty {
            copy::choose_folder()
        } else {
            copy::add_folder()
        });

        // Rebuild folder list rows only when visible text changes.
        let mut sig: Vec<String> = Vec::with_capacity(doc.folders.len());
        for f in &doc.folders {
            let w = solstone_linux::folders::folder_word(f);
            sig.push(format!(
                "{}|{}|{}|{}|{}|{:?}|{:?}",
                f.id,
                f.name,
                shell_model::state_label(w),
                f.facet.as_ref().map(|x| x.title.as_str()).unwrap_or(""),
                f.items_taken_in,
                f.progress,
                f.attention
            ));
        }
        if *self.list_signature.borrow() != sig {
            *self.list_signature.borrow_mut() = sig;
            while let Some(child) = self.list_rows_box.first_child() {
                self.list_rows_box.remove(&child);
            }
            for f in &doc.folders {
                let btn = self.build_folder_row(f);
                self.list_rows_box.append(&btn);
            }
        }
        self.list_rows_box.set_visible(!empty);

        // Pending path check card (AlreadyAdded / Overlap / Refused)
        let pending = self.pending_check.borrow().clone();
        match pending {
            Some(PathCheckResult::AlreadyAdded { name, .. }) => {
                self.check_heading.set_text(&copy::f21_notice(&name));
                self.check_body.set_visible(false);
                self.check_primary.set_label(&copy::f21_open(&name));
                self.check_primary.set_visible(true);
                self.check_secondary.set_visible(false);
                self.check_actions.set_visible(true);
                self.check_card.set_visible(true);
            }
            Some(PathCheckResult::Overlaps { target, .. }) => {
                let other = copy::overlap_target(&target);
                self.check_heading.set_text(&copy::overlap_heading(&other));
                self.check_body.set_text(copy::overlap_body());
                self.check_body.set_visible(true);
                self.check_primary.set_label(copy::overlap_choose());
                self.check_primary.set_visible(true);
                self.check_secondary.set_label(copy::overlap_override());
                self.check_secondary.set_visible(true);
                self.check_actions.set_visible(true);
                self.check_card.set_visible(true);
            }
            Some(PathCheckResult::Refused { .. }) => {
                self.check_heading.set_text(copy::refused());
                self.check_body.set_visible(false);
                self.check_primary.set_label(copy::choose_folder());
                self.check_primary.set_visible(true);
                self.check_secondary.set_visible(false);
                self.check_actions.set_visible(true);
                self.check_card.set_visible(true);
            }
            _ => {
                self.check_card.set_visible(false);
            }
        }

        // Aggregate intake totals
        let total = shell_model::folders_so_far(intake);
        let (started, last_raw) = match intake {
            IntakeFile::Rows(rows) => (
                rows.folders.started.as_deref(),
                rows.folders.last.as_deref(),
            ),
            IntakeFile::Missing | IntakeFile::Refused => (None, None),
        };
        let so_far = shell_model::page_total_now(&total, started);
        self.list_so_far_row.set_visible(!empty && so_far.is_some());
        self.list_so_far
            .set_text(so_far.as_deref().unwrap_or_default());
        let last = shell_model::source_last_now(intake, last_raw);
        let last = shell_model::last_text(&last);
        self.list_last_row.set_visible(!empty && last.is_some());
        self.list_last.set_text(last.unwrap_or_default());
    }

    fn build_folder_row(self: &Rc<Self>, folder: &FolderSummary) -> gtk::Button {
        let btn = gtk::Button::new();
        btn.add_css_class("sol-status-import");
        btn.set_has_frame(false);
        btn.set_halign(gtk::Align::Fill);

        let outer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let lines = gtk::Box::new(gtk::Orientation::Vertical, 2);
        lines.set_hexpand(true);

        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let name = gtk::Label::new(Some(&folder.name));
        name.add_css_class("sol-ink");
        name.add_css_class("sol-remote-device");
        name.set_halign(gtk::Align::Start);
        name.set_xalign(0.0);
        top.append(&name);
        if let Some(facet) = &folder.facet {
            let pill = gtk::Label::new(Some(&facet.title));
            pill.add_css_class("sol-faint");
            pill.add_css_class("sol-quiet");
            top.append(&pill);
        }
        lines.append(&top);

        let word = solstone_linux::folders::folder_word(folder);
        let sub_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let mark = mark_widget();
        paint_mark(&mark, word);
        let state = gtk::Label::new(Some(shell_model::state_label(word)));
        paint_word(&state, word);
        state.add_css_class("sol-quiet");
        sub_row.append(&mark);
        sub_row.append(&state);

        if let Some([done, total]) = folder.progress {
            let p = gtk::Label::new(Some(&copy::f10(&group(done), &group(total))));
            p.add_css_class("sol-faint");
            p.add_css_class("sol-quiet");
            p.set_wrap(true);
            p.set_xalign(0.0);
            sub_row.append(&p);
        } else if folder.items_taken_in > 0 {
            let count = gtk::Label::new(Some(&shell_model::format_items(folder.items_taken_in)));
            count.add_css_class("sol-faint");
            count.add_css_class("sol-quiet");
            sub_row.append(&count);
        }
        lines.append(&sub_row);

        let chev = gtk::Label::new(Some("\u{203a}"));
        chev.add_css_class("sol-faint");
        outer.append(&lines);
        outer.append(&chev);
        btn.set_child(Some(&outer));

        let id = folder.id.clone();
        let weak = Rc::downgrade(self);
        btn.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                s.open_detail(id.clone());
            }
        });
        btn
    }

    fn paint_detail(self: &Rc<Self>, doc: &FoldersDocument, folder: &FolderSummary) {
        self.painting.set(true);

        let word = solstone_linux::folders::folder_word(folder);
        self.detail_name.set_text(&folder.name);
        paint_mark(&self.detail_mark, word);
        self.detail_word.set_text(shell_model::state_label(word));
        paint_word(&self.detail_word, word);

        let setting_up = folder.mode == FolderMode::SettingUp;
        self.detail_switch.set_visible(!setting_up);
        if !setting_up {
            let on = matches!(folder.mode, FolderMode::On | FolderMode::Paused);
            if self.detail_switch.is_active() != on {
                self.detail_switch.set_active(on);
            }
        }

        // Pause / Resume buttons (never pause when computer-paused or off/setup)
        let show_pause = folder.mode == FolderMode::On
            && !folder.paused_by_computer
            && folder.attention.is_none();
        let show_resume = folder.mode == FolderMode::Paused && folder.attention.is_none();
        self.pause_button.set_visible(show_pause);
        self.resume_button.set_visible(show_resume);
        self.pause_row.set_visible(show_pause || show_resume);

        // Attention / Pause / Pairing / Journal capability banner
        let (banner, show_pick, show_retry) = match folder.attention {
            Some(FolderAttention::Missing) => (Some(copy::e1(&folder.name)), true, false),
            Some(FolderAttention::Unreadable) => (Some(copy::e2(&folder.name)), true, true),
            None if folder.mode == FolderMode::Paused => {
                (Some(copy::f15().to_owned()), false, false)
            }
            None if folder.mode == FolderMode::On && folder.paused_by_computer => {
                (Some(copy::f15b().to_owned()), false, false)
            }
            None if folder.mode == FolderMode::Off => (Some(copy::f16().to_owned()), false, false),
            None if folder.mode == FolderMode::On && !doc.paired => {
                (Some(copy::f12().to_owned()), false, false)
            }
            None if folder.mode == FolderMode::On && doc.journal_lists == Some(false) => {
                (Some(copy::f13().to_owned()), false, false)
            }
            _ => (None, false, false),
        };
        if let Some(msg) = banner {
            self.banner_text.set_text(&msg);
            self.banner_primary.set_visible(show_pick);
            self.banner_secondary.set_visible(show_retry);
            self.banner_box.set_visible(true);
        } else {
            self.banner_box.set_visible(false);
        }

        // Band 4 progress
        if let Some([done, total]) = folder.progress
            && folder.mode == FolderMode::On
        {
            self.progress_label
                .set_text(&copy::f10(&group(done), &group(total)));
            self.progress_label.set_visible(true);
        } else {
            self.progress_label.set_visible(false);
        }

        // Non-fatal unreadable files & content fallback notices
        if folder.cant_read > 0 {
            self.cant_read_label
                .set_text(&copy::cant_read(folder.cant_read));
            self.cant_read_box.set_visible(true);
        } else {
            self.cant_read_box.set_visible(false);
        }
        if folder.fallback_to_details > 0 {
            self.fallback_label
                .set_text(&copy::fallback_to_details(folder.fallback_to_details));
            self.fallback_label.set_visible(true);
        } else {
            self.fallback_label.set_visible(false);
        }

        // Kind checkboxes
        let flags = [
            folder.kinds.notes,
            folder.kinds.documents,
            folder.kinds.transcripts,
            folder.kinds.screenshots,
            folder.kinds.photos,
            folder.kinds.other,
        ];
        for (check, val) in self.kind_checks.iter().zip(flags) {
            if check.is_active() != val {
                check.set_active(val);
            }
        }
        self.kinds_error.set_visible(!folder.kinds.any_selected());
        self.kinds_note.set_visible(!setting_up);

        // Facet dropdown
        let mut facet_sig: Vec<String> = vec![copy::facet_none().to_owned()];
        for opt in &doc.facets {
            facet_sig.push(opt.title.clone());
        }
        if *self.facets_signature.borrow() != facet_sig {
            *self.facets_signature.borrow_mut() = facet_sig.clone();
            let refs: Vec<&str> = facet_sig.iter().map(String::as_str).collect();
            let list = gtk::StringList::new(&refs);
            self.facet_dropdown.set_model(Some(&list));
        }
        let selected_idx = folder
            .facet
            .as_ref()
            .and_then(|cur| doc.facets.iter().position(|o| o.name == cur.name))
            .map(|i| (i + 1) as u32)
            .unwrap_or(0);
        if self.facet_dropdown.selected() != selected_idx {
            self.facet_dropdown.set_selected(selected_idx);
        }
        let facets_ready = doc.facets_state == FacetsAvailability::Ready && !doc.facets.is_empty();
        self.facet_dropdown
            .set_sensitive(facets_ready && !self.busy.get());
        let facet_note = match doc.facets_state {
            FacetsAvailability::Unpaired => copy::facet_unpaired().to_owned(),
            FacetsAvailability::Offline => copy::facet_offline().to_owned(),
            FacetsAvailability::Ready => match &folder.facet {
                None => copy::facet_help_none().to_owned(),
                Some(f) if setting_up => copy::facet_help_setup(&f.title),
                Some(f) => copy::facet_help_active(&f.title),
            },
        };
        self.facet_help.set_text(&facet_note);

        // History / Backfill chooser
        let chooser_shown = setting_up || self.backfill_open.get();
        self.chooser_box.set_visible(chooser_shown);
        self.bring_older_button.set_visible(
            folder.mode == FolderMode::On && !chooser_shown && folder.attention.is_none(),
        );
        if chooser_shown {
            let span = self.span.get();
            if let Some(idx) = SPANS.iter().position(|s| *s == span)
                && !self.span_radios[idx].is_active()
            {
                self.span_radios[idx].set_active(true);
            }
            self.chooser_note.set_text(if setting_up {
                copy::nothing_until_confirm()
            } else {
                copy::no_repeat()
            });
            self.backfill_cancel.set_visible(!setting_up);

            let matching_preview = folder.preview.as_ref().filter(|p| p.span == Some(span));
            match matching_preview {
                None => {
                    self.preview_status.set_text(copy::checking_folder());
                    self.preview_split.set_visible(false);
                    self.preview_est_heading.set_visible(false);
                    self.preview_est_values.set_visible(false);
                    self.preview_est_gloss.set_visible(false);
                    self.preview_scope.set_visible(false);
                    self.preview_retry.set_visible(false);
                    self.confirm_button.set_label(copy::checking_folder());
                    self.confirm_button.set_sensitive(false);
                }
                Some(p) if p.failed => {
                    self.preview_status.set_text(copy::preview_failed());
                    self.preview_split.set_visible(false);
                    self.preview_est_heading.set_visible(false);
                    self.preview_est_values.set_visible(false);
                    self.preview_est_gloss.set_visible(false);
                    self.preview_scope.set_visible(false);
                    self.preview_retry.set_visible(true);
                    self.confirm_button.set_sensitive(false);
                }
                Some(p) => {
                    self.preview_retry.set_visible(false);
                    if p.total_items == 0 {
                        self.preview_status.set_text(copy::preview_zero());
                        self.preview_split.set_visible(false);
                        self.preview_est_heading.set_visible(false);
                        self.preview_est_values.set_visible(false);
                        self.preview_est_gloss.set_visible(false);
                    } else {
                        let status_line = match (p.oldest_ms, p.newest_ms) {
                            (Some(old), Some(new)) => {
                                let start = format_local_date(old);
                                let end = format_local_date(new);
                                if !start.is_empty() && !end.is_empty() {
                                    copy::preview_span(&group(p.total_items), &start, &end)
                                } else {
                                    copy::preview_count(&group(p.total_items))
                                }
                            }
                            _ => copy::preview_count(&group(p.total_items)),
                        };
                        self.preview_status.set_text(&status_line);

                        let split = match (p.content_items > 0, p.metadata_only_items > 0) {
                            (true, true) => Some(copy::preview_both(
                                &group(p.content_items),
                                &format_bytes(p.content_bytes),
                                &group(p.metadata_only_items),
                            )),
                            (true, false) => Some(copy::preview_content_only(
                                &group(p.content_items),
                                &format_bytes(p.content_bytes),
                            )),
                            (false, true) => {
                                Some(copy::preview_meta_only(&group(p.metadata_only_items)))
                            }
                            (false, false) => None,
                        };
                        if let Some(s) = split {
                            self.preview_split.set_text(&s);
                            self.preview_split.set_visible(true);
                        } else {
                            self.preview_split.set_visible(false);
                        }

                        self.preview_est_heading.set_visible(true);
                        if !doc.paired {
                            self.preview_est_values.set_text(copy::est_unpaired());
                            self.preview_est_values.set_visible(true);
                            self.preview_est_gloss.set_visible(false);
                        } else if doc.offline {
                            self.preview_est_values.set_text(copy::est_offline());
                            self.preview_est_values.set_visible(true);
                            self.preview_est_gloss.set_visible(false);
                        } else if let Some(est) = &p.estimate {
                            self.preview_est_values.set_text(&copy::est_values(
                                &group(est.segments),
                                &group(est.days),
                                &group(est.thoughts),
                            ));
                            self.preview_est_values.set_visible(true);
                            self.preview_est_gloss.set_visible(true);
                        } else {
                            self.preview_est_values.set_visible(false);
                            self.preview_est_gloss.set_visible(false);
                        }
                    }
                    self.preview_scope.set_visible(true);
                    self.confirm_button
                        .set_label(&copy::confirm_label(p.total_items, span == Span::Today));
                    self.confirm_button
                        .set_sensitive(folder.kinds.any_selected() && !self.busy.get());
                }
            }
        }

        // Facts
        self.path_value.set_text(&folder.path);
        self.change_folder_button.set_visible(!show_pick);
        if let Some(overlap) = &folder.overlap {
            let other = copy::overlap_target(overlap);
            self.overlap_fact_label
                .set_text(&copy::overlap_fact(&other));
            self.overlap_fact_label.set_visible(true);
        } else {
            self.overlap_fact_label.set_visible(false);
        }
        self.detail_so_far_row
            .set_visible(folder.items_taken_in > 0);
        if folder.items_taken_in > 0 {
            self.detail_so_far
                .set_text(&shell_model::format_items(folder.items_taken_in));
        }
        let last = folder
            .last_taken_in_ms
            .and_then(|ms| shell_model::relative_unix_now(ms as f64 / 1000.0));
        self.detail_last_row.set_visible(last.is_some());
        self.detail_last
            .set_text(last.as_deref().unwrap_or_default());
        let checked = folder
            .last_checked_ms
            .and_then(|ms| shell_model::relative_unix_now(ms as f64 / 1000.0));
        self.detail_checked_row.set_visible(checked.is_some());
        self.detail_checked
            .set_text(checked.as_deref().unwrap_or_default());
        self.detail_waiting_row.set_visible(folder.waiting > 0);
        self.detail_waiting.set_text(&group(folder.waiting));
        self.detail_other_note.set_text(if folder.kinds.other {
            copy::f18()
        } else {
            copy::f18b()
        });

        // Remove folder inline confirm
        let confirming_remove = self.remove_confirm_open.get();
        self.remove_button.set_visible(!confirming_remove);
        self.remove_card.set_visible(confirming_remove);
        if confirming_remove {
            self.remove_heading
                .set_text(&copy::f23_heading(&folder.name));
            self.remove_body.set_text(&copy::f23_body(&folder.name));
        }

        self.painting.set(false);
    }
}

async fn check_path_dbus(path: &str) -> Option<PathCheckResult> {
    let client = solstone_linux::dbus_client::ControlClient::session()
        .await
        .ok()?;
    let raw = client.folders_check_path(path).await.ok()?;
    serde_json::from_str(&raw).ok()
}

async fn send_action_dbus(req: &FolderActionRequest) -> Option<String> {
    let client = solstone_linux::dbus_client::ControlClient::session()
        .await
        .ok()?;
    let payload = serde_json::to_string(req).ok()?;
    client.folders_action(&payload).await.ok()
}
