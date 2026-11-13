// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The screenshots page under its verdict: the reason and its one action (the
//! chooser while setting up), what it takes in, and its facts.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use solstone_linux::copy::screenshots as copy;
use solstone_linux::screenshots::{Mode, ScreenshotsDocument, Span, group};
use solstone_linux::shell_model;
use solstone_linux::source_intake::IntakeFile;

use crate::deck::{fact_row, note_label};

/// The five choices, in calendar's order, beside their copy.
const SPANS: [Span; 5] = [
    Span::Last30Days,
    Span::Last6Months,
    Span::LastYear,
    Span::All,
    Span::Today,
];

#[derive(Clone)]
enum Op {
    TurnOn,
    TurnOff,
    ChooseFolder(String),
    Count(Span),
    Confirm(Span),
    Retry,
}

pub struct ScreenshotsSection {
    pub root: gtk::Box,
    reason: gtk::Label,
    reason_action: gtk::Button,
    chooser: gtk::Box,
    choices: Vec<gtk::CheckButton>,
    returning: gtk::Label,
    confirm: gtk::Button,
    count_failed: gtk::Box,
    folder_note: gtk::Label,
    so_far_row: gtk::Box,
    so_far: gtk::Label,
    last_row: gtk::Box,
    last: gtk::Label,
    progress: gtk::Label,
    folder_row: gtk::Box,
    folder: gtk::Label,
    folder_button: gtk::Button,
    checked_row: gtk::Box,
    checked: gtk::Label,
    waiting_row: gtk::Box,
    waiting: gtk::Label,
    cant_come_in: gtk::Label,
    cant_read: gtk::Box,
    cant_read_text: gtk::Label,
    span: Cell<Span>,
    busy: Cell<bool>,
    painting: Cell<bool>,
    /// The page is showing, so a count may be asked for.
    shown: Cell<bool>,
    doc: RefCell<Option<ScreenshotsDocument>>,
    /// The folder picker has to outlive the click, or it closes at once.
    picker: RefCell<Option<gtk::FileChooserNative>>,
}

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

impl ScreenshotsSection {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_visible(false);

        let reason = ink(note_label(None));
        let reason_action = action(copy::CHOOSE_FOLDER);
        reason_action.set_visible(false);
        root.append(&reason);
        root.append(&reason_action);

        let chooser = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let heading = crate::deck::section_label(copy::S7);
        heading.add_css_class("sol-rule");
        chooser.append(&heading);
        let mut choices: Vec<gtk::CheckButton> = Vec::new();
        for text in copy::S8 {
            let choice = gtk::CheckButton::with_label(text);
            choice.add_css_class("sol-ink");
            choice.add_css_class("sol-choice");
            if let Some(first) = choices.first() {
                choice.set_group(Some(first));
            }
            chooser.append(&choice);
            choices.push(choice);
        }
        choices[0].set_active(true);
        chooser.append(&note_label(Some(copy::S9)));
        let returning = note_label(Some(copy::S17));
        chooser.append(&returning);
        let confirm = action(copy::S10_COUNTING);
        chooser.append(&confirm);
        let count_failed = gtk::Box::new(gtk::Orientation::Vertical, 6);
        count_failed.append(&ink(note_label(Some(copy::S12))));
        let count_again = action(copy::S12_ACTION);
        count_failed.append(&count_again);
        count_failed.set_visible(false);
        chooser.append(&count_failed);
        chooser.set_visible(false);
        root.append(&chooser);

        root.append(&note_label(Some(copy::D1)));
        let folder_note = note_label(None);
        root.append(&folder_note);
        root.append(&note_label(Some(copy::S14)));
        root.append(&note_label(Some(copy::S15)));
        root.append(&note_label(Some(copy::S23)));

        let (so_far_row, so_far) = fact_row("so far");
        let (last_row, last) = fact_row("last taken in");
        root.append(&so_far_row);
        root.append(&last_row);
        let progress = note_label(None);
        root.append(&progress);
        let (folder_row, folder) = fact_row(copy::S22);
        let folder_button = action(copy::CHOOSE_FOLDER);
        let folder_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        folder_box.append(&folder_row);
        folder_box.append(&folder_button);
        root.append(&folder_box);
        let (checked_row, checked) = fact_row(copy::S18);
        root.append(&checked_row);
        let (waiting_row, waiting) = fact_row(copy::S19);
        root.append(&waiting_row);
        let cant_come_in = ink(note_label(None));
        root.append(&cant_come_in);
        let cant_read = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let cant_read_text = ink(note_label(None));
        let read_again = action(copy::S12_ACTION);
        cant_read.append(&cant_read_text);
        cant_read.append(&read_again);
        root.append(&cant_read);

        let more = gtk::Expander::new(Some(copy::D15));
        more.add_css_class("sol-ink");
        more.set_child(Some(&note_label(Some(copy::D10))));
        root.append(&more);

        let section = Rc::new(Self {
            root,
            reason,
            reason_action,
            chooser,
            choices,
            returning,
            confirm,
            count_failed,
            folder_note,
            so_far_row,
            so_far,
            last_row,
            last,
            progress,
            folder_row,
            folder,
            folder_button,
            checked_row,
            checked,
            waiting_row,
            waiting,
            cant_come_in,
            cant_read,
            cant_read_text,
            span: Cell::new(Span::Last30Days),
            busy: Cell::new(false),
            painting: Cell::new(false),
            shown: Cell::new(false),
            doc: RefCell::new(None),
            picker: RefCell::new(None),
        });

        for (choice, span) in section.choices.iter().zip(SPANS) {
            let weak = Rc::downgrade(&section);
            choice.connect_toggled(move |choice| {
                let Some(section) = weak.upgrade() else {
                    return;
                };
                if choice.is_active() && !section.painting.get() {
                    section.span.set(span);
                    section.run(Op::Count(span));
                }
            });
        }
        {
            let weak = Rc::downgrade(&section);
            section.confirm.connect_clicked(move |_| {
                let Some(section) = weak.upgrade() else {
                    return;
                };
                let span = section.span.get();
                let counted = section
                    .doc
                    .borrow()
                    .as_ref()
                    .and_then(|doc| doc.count)
                    .is_some_and(|count| count.span == span);
                if counted {
                    section.run(Op::Confirm(span));
                }
            });
        }
        {
            let weak = Rc::downgrade(&section);
            count_again.connect_clicked(move |_| {
                if let Some(section) = weak.upgrade() {
                    section.run(Op::Count(section.span.get()));
                }
            });
        }
        {
            let weak = Rc::downgrade(&section);
            read_again.connect_clicked(move |_| {
                if let Some(section) = weak.upgrade() {
                    section.run(Op::Retry);
                }
            });
        }
        for button in [&section.reason_action, &section.folder_button] {
            let weak = Rc::downgrade(&section);
            button.connect_clicked(move |button| {
                if let Some(section) = weak.upgrade() {
                    section.pick_folder(button);
                }
            });
        }
        section
    }

    /// The page's switch, while on or off. Setting up has no switch: the confirm
    /// is what turns it on.
    pub fn switch(&self) -> Option<bool> {
        let doc = self.doc.borrow();
        match doc.as_ref()?.mode {
            Mode::On => Some(true),
            Mode::Off => Some(false),
            Mode::NotSetUp | Mode::Choosing => None,
        }
    }

    pub fn set_on(self: &Rc<Self>, on: bool) {
        self.run(if on { Op::TurnOn } else { Op::TurnOff });
    }

    pub fn set_shown(self: &Rc<Self>, shown: bool) {
        self.shown.set(shown);
        if shown {
            self.ask_count();
        }
    }

    fn pick_folder(self: &Rc<Self>, button: &gtk::Button) {
        let picker = gtk::FileChooserNative::new(
            Some(copy::D8_PICKER_TITLE),
            button.root().and_downcast::<gtk::Window>().as_ref(),
            gtk::FileChooserAction::SelectFolder,
            Some(solstone_linux::copy::OBSIDIAN_CHOOSER_ACCEPT),
            Some(solstone_linux::copy::OBSIDIAN_CANCEL),
        );
        let weak = Rc::downgrade(self);
        picker.connect_response(move |picker, response| {
            if let Some(section) = weak.upgrade() {
                if response == gtk::ResponseType::Accept
                    && let Some(path) = picker.file().and_then(|file| file.path())
                    && let Some(path) = path.to_str()
                {
                    section.run(Op::ChooseFolder(path.to_owned()));
                }
                section.picker.borrow_mut().take();
            }
            picker.destroy();
        });
        picker.show();
        *self.picker.borrow_mut() = Some(picker);
    }

    /// Count for the chosen span when the chooser shows and has no count for it.
    fn ask_count(self: &Rc<Self>) {
        if !self.shown.get() || self.busy.get() {
            return;
        }
        let needs = self.doc.borrow().as_ref().is_some_and(|doc| {
            matches!(doc.mode, Mode::NotSetUp | Mode::Choosing)
                && doc.folder.is_some()
                && !doc.count_failed
                && doc.count.is_none_or(|count| count.span != self.span.get())
        });
        if needs {
            self.run(Op::Count(self.span.get()));
        }
    }

    fn run(self: &Rc<Self>, op: Op) {
        if self.busy.get() {
            return;
        }
        self.busy.set(true);
        self.paint_busy();
        let section = Rc::clone(self);
        glib::MainContext::default().spawn_local(async move {
            let answer = request(op).await;
            section.busy.set(false);
            if let Some(doc) = answer
                .as_deref()
                .and_then(solstone_linux::screenshots::decode_document)
            {
                crate::deck::take_screenshots(doc);
            } else {
                section.paint_busy();
            }
        });
    }

    fn paint_busy(&self) {
        let busy = self.busy.get();
        for choice in &self.choices {
            choice.set_sensitive(!busy);
        }
        self.folder_button.set_sensitive(!busy);
        self.reason_action.set_sensitive(!busy);
        if busy {
            self.confirm.set_label(copy::S10_COUNTING);
            self.confirm.set_sensitive(false);
        }
    }

    /// Show what the running app says, with this install's totals.
    pub fn update(self: &Rc<Self>, doc: Option<&ScreenshotsDocument>, intake: &IntakeFile) {
        *self.doc.borrow_mut() = doc.cloned();
        let Some(doc) = doc else {
            self.root.set_visible(false);
            return;
        };
        self.root.set_visible(true);
        let setting_up = matches!(doc.mode, Mode::NotSetUp | Mode::Choosing);

        // The reason, and its one action.
        let reason: Option<String> = match doc.mode {
            Mode::NotSetUp | Mode::Choosing if doc.folder.is_none() => Some(copy::D8.to_owned()),
            Mode::On if doc.folder_gone => doc.folder_name.as_deref().map(copy::d9),
            Mode::On if doc.paused => Some(copy::S16_COMPUTER.to_owned()),
            Mode::On if doc.journal_lists == Some(false) => Some(copy::S21.to_owned()),
            _ => None,
        };
        let folder_action =
            (setting_up && doc.folder.is_none()) || (doc.mode == Mode::On && doc.folder_gone);
        match &reason {
            Some(text) => {
                self.reason.set_text(text);
                self.reason.set_visible(true);
            }
            None => self.reason.set_visible(false),
        }
        self.reason_action.set_visible(folder_action);

        // The chooser.
        let choosing = setting_up && doc.folder.is_some();
        self.chooser.set_visible(choosing);
        if choosing {
            self.returning.set_visible(doc.returning);
            if let Some(count) = doc.count {
                self.painting.set(true);
                if let Some(index) = SPANS.iter().position(|span| *span == count.span) {
                    self.choices[index].set_active(true);
                    self.span.set(count.span);
                }
                self.painting.set(false);
            }
            let counted = doc.count.filter(|count| count.span == self.span.get());
            self.count_failed
                .set_visible(doc.count_failed && !self.busy.get());
            self.confirm
                .set_visible(!doc.count_failed || self.busy.get());
            match counted {
                Some(count) if !self.busy.get() => {
                    let label = if self.span.get() == Span::Today {
                        copy::S10_TODAY.to_owned()
                    } else {
                        copy::s10(count.n, &group(count.n))
                    };
                    self.confirm.set_label(&label);
                    self.confirm.set_sensitive(true);
                }
                _ => {
                    self.confirm.set_label(copy::S10_COUNTING);
                    self.confirm.set_sensitive(false);
                }
            }
        }

        // What stays where it is: a folder the owner chose names itself.
        match (doc.folder_chosen, doc.folder_name.as_deref()) {
            (true, Some(name)) => {
                self.folder_note.set_text(&copy::d3(name));
                self.folder_note.set_visible(true);
            }
            _ => self.folder_note.set_visible(false),
        }

        // This install's totals.
        let total = shell_model::screenshots_so_far(intake);
        let (started, last_raw) = match intake {
            IntakeFile::Rows(rows) => (
                rows.screenshots.started.as_deref(),
                rows.screenshots.last.as_deref(),
            ),
            IntakeFile::Missing | IntakeFile::Refused => (None, None),
        };
        let so_far = shell_model::page_total_now(&total, started);
        self.so_far_row.set_visible(so_far.is_some());
        self.so_far.set_text(so_far.as_deref().unwrap_or_default());
        let last = shell_model::source_last_now(intake, last_raw);
        let last = shell_model::last_text(&last);
        self.last_row.set_visible(last.is_some());
        self.last.set_text(last.unwrap_or_default());

        match doc.progress {
            Some([done, total]) if doc.mode == Mode::On => {
                self.progress
                    .set_text(&copy::s6(&group(done), &group(total)));
                self.progress.set_visible(true);
            }
            _ => self.progress.set_visible(false),
        }
        self.folder_row.set_visible(doc.folder.is_some());
        self.folder
            .set_text(doc.folder.as_deref().unwrap_or_default());
        // The reason carries the folder's action when there's no folder to show.
        self.folder_button
            .set_visible(doc.folder.is_some() && !folder_action);
        let checked = doc
            .last_checked_ms
            .and_then(|ms| shell_model::relative_unix_now(ms as f64 / 1000.0));
        self.checked_row.set_visible(checked.is_some());
        self.checked
            .set_text(checked.as_deref().unwrap_or_default());
        self.waiting_row.set_visible(doc.waiting > 0);
        self.waiting.set_text(&group(doc.waiting));
        self.cant_come_in
            .set_text(&copy::d12(doc.cant_come_in, &group(doc.cant_come_in)));
        self.cant_come_in.set_visible(doc.cant_come_in > 0);
        self.cant_read_text
            .set_text(&copy::d13(doc.cant_read, &group(doc.cant_read)));
        self.cant_read.set_visible(doc.cant_read > 0);
        self.paint_busy();
        self.ask_count();
    }
}

async fn request(op: Op) -> Option<String> {
    let client = solstone_linux::dbus_client::ControlClient::session()
        .await
        .ok()?;
    let answer = match op {
        Op::TurnOn => client.screenshots_turn_on().await,
        Op::TurnOff => client.screenshots_turn_off().await,
        Op::ChooseFolder(path) => client.screenshots_choose_folder(&path).await,
        Op::Count(span) => client.screenshots_count(span.as_str()).await,
        Op::Confirm(span) => client.screenshots_confirm(span.as_str()).await,
        Op::Retry => client.screenshots_retry().await,
    };
    answer.ok()
}
