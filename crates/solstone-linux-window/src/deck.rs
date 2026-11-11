// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The deck, the shelf, and the status pane on the sun-arc chrome.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use solstone_linux::config::{self, OwnerSwitch};
use solstone_linux::desktop_session::SessionPresence;
use solstone_linux::pause_hold::{self, HeldPause};
use solstone_linux::shell_model::{
    self, BrowserHost, ObsidianFacts, PauseFor, PauseLeft, ScreenSub, StateWord, TileId, WidthClass,
};
use solstone_linux::source_intake::{self, IntakeFile, IntakeRows};
use solstone_linux::terminal::state::{EngineState, TerminalState};
use solstone_linux::tray_model::TrayStatus;

use crate::sun_bg::{self, Chrome};

const PANE_WIDTH: i32 = 360;
const OFF_SUB: &str = "turn it on any time.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShelfRow {
    Home,
    Journal,
    Computer,
    Updates,
    Help,
}

struct View {
    screen_on: bool,
    terminal_on: bool,
    screen: StateWord,
    screen_sub: ScreenSub,
    terminal: StateWord,
    terminal_reason: Option<&'static str>,
    browsers: StateWord,
    browsers_reason: Option<&'static str>,
    browsers_sub: Option<String>,
    obsidian: StateWord,
    intake: IntakeFile,
    pause: Option<PauseLeft>,
    paired: bool,
    /// Paired, and the service health is offline. A waiting count is separate.
    offline: bool,
    offline_waiting: Option<i64>,
    kept: String,
}

#[derive(Clone)]
struct ServiceSnap {
    ready: bool,
    capture: String,
    capture_attention: bool,
    activity_source: bool,
    durable: String,
    health: String,
    queue: Option<i64>,
    obsidian: Option<solstone_linux::obsidian::State>,
    obsidian_vault: bool,
    kept: String,
}

impl ServiceSnap {
    fn unread() -> Self {
        Self {
            ready: false,
            capture: String::new(),
            capture_attention: false,
            activity_source: true,
            durable: String::new(),
            health: String::new(),
            queue: None,
            obsidian: None,
            obsidian_vault: false,
            kept: "nothing yet".to_owned(),
        }
    }
}

struct TileWidgets {
    root: gtk::Box,
    word: gtk::Label,
    sub: gtk::Label,
    mark: gtk::DrawingArea,
    switch: Option<gtk::Switch>,
    id: TileId,
}

struct FactWidgets {
    so_far: gtk::Label,
    last: gtk::Label,
}

pub fn mount(window: &gtk::ApplicationWindow, chrome: &Chrome) {
    install_deck_css(window);
    let seen_report = Rc::new(Cell::new(false));
    let daemon_up = Rc::new(Cell::new(None));
    let snap = Rc::new(RefCell::new(ServiceSnap::unread()));
    SERVICE.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&snap)));
    HOST_UP.with(|cell| cell.set(None));
    let view = Rc::new(RefCell::new(read_view(&seen_report, daemon_up.get())));
    let selected: Rc<Cell<Option<TileId>>> = Rc::new(Cell::new(None));
    let row = Rc::new(Cell::new(ShelfRow::Home));
    let shelf_open = Rc::new(Cell::new(false));
    let page_open = Rc::new(Cell::new(false));

    let greet = gtk::Label::new(Some(shell_model::greeting_now()));
    greet.add_css_class("sol-greet");
    greet.add_css_class("sol-display");
    greet.add_css_class("sol-ink");
    greet.set_halign(gtk::Align::Start);
    greet.set_xalign(0.0);
    let pause_line = gtk::Label::new(None);
    pause_line.add_css_class("sol-faint");
    pause_line.set_halign(gtk::Align::Start);
    pause_line.set_visible(false);
    let greet_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    greet_text.append(&greet);
    greet_text.append(&pause_line);

    let (pill, pill_mark, pill_label) = status_pill();
    let greet_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    greet_row.append(&greet_text);
    let greet_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    greet_spacer.set_hexpand(true);
    greet_row.append(&greet_spacer);
    let (pause_button, resume_button) = pause_controls();
    greet_row.append(&pause_button);
    greet_row.append(&resume_button);
    greet_row.append(&pill);

    let grid = gtk::Grid::new();
    grid.set_column_spacing(12);
    grid.set_row_spacing(12);
    grid.set_column_homogeneous(true);
    grid.set_hexpand(true);
    grid.set_valign(gtk::Align::Start);

    let screen = make_tile("screen", GLYPH_SCREEN, Some(view.borrow().screen_on), false);
    let terminal = make_tile(
        "terminal",
        GLYPH_TERMINAL,
        Some(view.borrow().terminal_on),
        false,
    );
    let browsers = make_tile("browsers", GLYPH_BROWSERS, None, false);
    let obsidian = make_tile("Obsidian", GLYPH_OBSIDIAN, None, false);
    let (add_more, _, _, _, _) = make_tile("add more", GLYPH_PLUS, None, true);
    let tiles = vec![
        tile_widgets(screen, TileId::Screen),
        tile_widgets(terminal, TileId::Terminal),
        tile_widgets(browsers, TileId::Browsers),
        tile_widgets(obsidian, TileId::Obsidian),
    ];

    let built = make_pane();
    place_pane(&chrome.body, &built.root);
    let (journal, journal_label) = journal_pill();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_hexpand(true);
    column.set_vexpand(true);
    column.append(&greet_row);
    column.append(&grid);
    let foot_spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    foot_spacer.set_vexpand(true);
    column.append(&foot_spacer);
    column.append(&journal);

    let other = gtk::Label::new(None);
    other.add_css_class("sol-name");
    other.add_css_class("sol-display");
    other.add_css_class("sol-ink");
    other.set_halign(gtk::Align::Start);
    other.set_valign(gtk::Align::Start);
    other.set_visible(false);

    chrome.body.append(&column);
    chrome.body.append(&other);

    let shelf_rows = fill_shelf(&chrome.shelf);
    let shelf_button = gtk::Button::from_icon_name("sidebar-show-symbolic");
    shelf_button.set_tooltip_text(Some("sidebar"));
    shelf_button.set_visible(false);
    chrome.header_extra.append(&shelf_button);

    let ui = Rc::new(Ui {
        window: window.clone(),
        shelf: chrome.shelf.clone(),
        body: chrome.body.clone(),
        grid: grid.clone(),
        other: other.clone(),
        pane: built.root.clone(),
        greet,
        pause_line,
        pill_mark,
        pill_label,
        pause_button,
        resume_button,
        shelf_button: shelf_button.clone(),
        tiles,
        add_more,
        facts: built.facts,
        lead: built.lead,
        soft: built.soft,
        kept: built.kept,
        unread: built.unread,
        page: built.page,
        shelf_rows,
        journal,
        journal_label,
    });

    for tile in &ui.tiles {
        let id = tile.id;
        let ui = Rc::clone(&ui);
        let selected = Rc::clone(&selected);
        let page_open = Rc::clone(&page_open);
        let row = Rc::clone(&row);
        let shelf_open = Rc::clone(&shelf_open);
        let view = Rc::clone(&view);
        let click = gtk::GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            row.set(ShelfRow::Home);
            selected.set(Some(id));
            page_open.set(true);
            ui.page.show_tile(id, &view.borrow());
            apply_layout(&ui, &selected, &row, &shelf_open, &page_open);
        });
        tile.root.add_controller(click);
    }

    {
        let ui_back = Rc::clone(&ui);
        let selected = Rc::clone(&selected);
        let page_open = Rc::clone(&page_open);
        let row = Rc::clone(&row);
        let shelf_open = Rc::clone(&shelf_open);
        let back: Rc<dyn Fn()> = Rc::new(move || {
            row.set(ShelfRow::Home);
            selected.set(None);
            page_open.set(false);
            ui_back.page.show_status();
            apply_layout(&ui_back, &selected, &row, &shelf_open, &page_open);
        });
        let back_pill = Rc::clone(&back);
        pill.connect_clicked(move |_| back_pill());
        let back_close = Rc::clone(&back);
        let close = ui.page.close.clone();
        close.connect_clicked(move |_| back_close());
        SHOW_HOME.with(|slot| *slot.borrow_mut() = Some(back));
    }

    for shelf in &ui.shelf_rows {
        let button = shelf.button.clone();
        let which = shelf.which;
        let ui = Rc::clone(&ui);
        let row = Rc::clone(&row);
        let selected = Rc::clone(&selected);
        let page_open = Rc::clone(&page_open);
        let shelf_open = Rc::clone(&shelf_open);
        button.connect_clicked(move |_| {
            row.set(which);
            if which == ShelfRow::Home {
                selected.set(None);
                page_open.set(false);
                ui.page.show_status();
            }
            if shelf_button_mode(ui.window.width()) {
                shelf_open.set(false);
            }
            apply_layout(&ui, &selected, &row, &shelf_open, &page_open);
        });
    }

    {
        let ui = Rc::clone(&ui);
        let shelf_open = Rc::clone(&shelf_open);
        let selected = Rc::clone(&selected);
        let row = Rc::clone(&row);
        let page_open = Rc::clone(&page_open);
        shelf_button.connect_clicked(move |_| {
            shelf_open.set(!shelf_open.get());
            apply_layout(&ui, &selected, &row, &shelf_open, &page_open);
        });
    }

    wire_switch(
        ui.tiles[0].switch.as_ref(),
        OwnerSwitch::Screen,
        Rc::clone(&view),
        Rc::clone(&seen_report),
        Rc::clone(&daemon_up),
        Rc::clone(&ui),
        Rc::clone(&selected),
    );
    wire_switch(
        ui.tiles[1].switch.as_ref(),
        OwnerSwitch::Terminal,
        Rc::clone(&view),
        Rc::clone(&seen_report),
        Rc::clone(&daemon_up),
        Rc::clone(&ui),
        Rc::clone(&selected),
    );
    wire_pause(&ui, &view, &seen_report, &daemon_up, &selected);

    apply_view(&ui, &view.borrow());
    apply_layout(&ui, &selected, &row, &shelf_open, &page_open);

    let view_tick = Rc::clone(&view);
    let seen_tick = Rc::clone(&seen_report);
    let daemon_tick = Rc::clone(&daemon_up);
    let ui_tick = Rc::clone(&ui);
    let selected_tick = Rc::clone(&selected);
    glib::timeout_add_local(Duration::from_secs(2), move || {
        if !ui_tick.window.is_mapped() {
            return glib::ControlFlow::Continue;
        }
        refresh(
            &ui_tick,
            &view_tick,
            &seen_tick,
            &daemon_tick,
            &selected_tick,
        );
        apply_layout(&ui_tick, &selected_tick, &row, &shelf_open, &page_open);
        glib::ControlFlow::Continue
    });

    probe_daemon(
        Rc::clone(&daemon_up),
        Rc::clone(&view),
        Rc::clone(&seen_report),
        Rc::clone(&ui),
        Rc::clone(&selected),
    );
    watch_service(snap, Rc::clone(&view), seen_report, daemon_up, ui, selected);
}

thread_local! {
    static SERVICE: RefCell<Option<Rc<RefCell<ServiceSnap>>>> = const { RefCell::new(None) };
    static HOST_UP: Cell<Option<bool>> = const { Cell::new(None) };
}

thread_local! {
    static SHOW_HOME: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

pub fn show_home() {
    SHOW_HOME.with(|slot| {
        if let Some(show) = slot.borrow().as_ref() {
            show();
        }
    });
}

struct Ui {
    window: gtk::ApplicationWindow,
    shelf: gtk::Box,
    body: gtk::Box,
    grid: gtk::Grid,
    other: gtk::Label,
    pane: gtk::Box,
    greet: gtk::Label,
    pause_line: gtk::Label,
    pill_mark: gtk::DrawingArea,
    pill_label: gtk::Label,
    pause_button: gtk::MenuButton,
    resume_button: gtk::Button,
    shelf_button: gtk::Button,
    tiles: Vec<TileWidgets>,
    add_more: gtk::Box,
    facts: [FactWidgets; 4],
    lead: gtk::Label,
    soft: gtk::Label,
    kept: gtk::Label,
    unread: gtk::Label,
    page: Page,
    shelf_rows: Vec<ShelfWidgets>,
    journal: gtk::Box,
    journal_label: gtk::Label,
}

struct ShelfWidgets {
    button: gtk::Button,
    which: ShelfRow,
    badge: gtk::Widget,
}

struct Page {
    status: gtk::Box,
    box_: gtk::Box,
    title: gtk::Label,
    word: gtk::Label,
    reason: gtk::Label,
    close: gtk::Button,
    so_far: gtk::Label,
    last: gtk::Label,
}

impl Page {
    fn show_status(&self) {
        self.box_.set_visible(false);
        self.status.set_visible(true);
    }

    fn show_tile(&self, id: TileId, view: &View) {
        let (word, reason, sub) = match id {
            TileId::Screen => (
                view.screen,
                None,
                shell_model::screen_sub_label(view.screen_sub),
            ),
            TileId::Terminal => (view.terminal, view.terminal_reason, None),
            TileId::Browsers => (
                view.browsers,
                view.browsers_reason,
                view.browsers_sub.as_deref(),
            ),
            TileId::Obsidian => (view.obsidian, None, None),
        };
        self.title.set_text(shell_model::tile_name(id));
        self.word.set_text(shell_model::state_label(word));
        paint_word(&self.word, word);
        match reason.or(sub) {
            Some(text) => {
                self.reason.set_text(text);
                self.reason.set_visible(true);
            }
            None => self.reason.set_visible(false),
        }
        let (so_far, last) = facts_for(id, &view.intake);
        set_fact(&self.so_far, so_far.as_deref());
        set_fact(&self.last, last.as_deref());
        self.status.set_visible(false);
        self.box_.set_visible(true);
    }
}

fn tile_widgets(
    built: (
        gtk::Box,
        gtk::Label,
        gtk::Label,
        gtk::DrawingArea,
        Option<gtk::Switch>,
    ),
    id: TileId,
) -> TileWidgets {
    TileWidgets {
        root: built.0,
        word: built.1,
        sub: built.2,
        mark: built.3,
        switch: built.4,
        id,
    }
}

fn make_tile(
    name: &str,
    glyph: &str,
    switch_on: Option<bool>,
    dashed: bool,
) -> (
    gtk::Box,
    gtk::Label,
    gtk::Label,
    gtk::DrawingArea,
    Option<gtk::Switch>,
) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.add_css_class("sol-tile");
    if dashed {
        root.add_css_class("sol-dash");
    }
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&glyph_widget(glyph));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    top.append(&spacer);
    let switch = switch_on.map(|on| {
        let switch = gtk::Switch::new();
        switch.set_active(on);
        switch.set_valign(gtk::Align::Center);
        switch.add_css_class("sol-switch");
        let eat = gtk::GestureClick::new();
        eat.connect_pressed(|gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        switch.add_controller(eat);
        top.append(&switch);
        switch
    });
    let title = gtk::Label::new(Some(name));
    title.add_css_class("sol-name");
    title.add_css_class("sol-display");
    title.add_css_class("sol-ink");
    title.set_halign(gtk::Align::Start);
    title.set_xalign(0.0);
    let state = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let mark = mark_widget();
    let word = gtk::Label::new(Some(shell_model::state_label(StateWord::Checking)));
    word.add_css_class("sol-faint");
    word.set_halign(gtk::Align::Start);
    word.set_xalign(0.0);
    state.append(&mark);
    state.append(&word);
    if dashed {
        state.set_visible(false);
    }
    let sub = gtk::Label::new(if dashed { Some("sources") } else { None });
    sub.add_css_class("sol-faint");
    sub.set_halign(gtk::Align::Start);
    sub.set_xalign(0.0);
    sub.set_wrap(true);
    sub.set_visible(dashed);
    root.append(&top);
    root.append(&title);
    root.append(&state);
    root.append(&sub);
    (root, word, sub, mark, switch)
}

struct BuiltPane {
    root: gtk::Box,
    facts: [FactWidgets; 4],
    lead: gtk::Label,
    soft: gtk::Label,
    kept: gtk::Label,
    unread: gtk::Label,
    page: Page,
}

fn make_pane() -> BuiltPane {
    let pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    pane.add_css_class("sol-pane");
    pane.set_size_request(PANE_WIDTH, -1);

    let status = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let title = gtk::Label::new(Some("status"));
    title.add_css_class("sol-display");
    title.add_css_class("sol-ink");
    title.set_halign(gtk::Align::Start);
    let lead = gtk::Label::new(Some("not paired"));
    lead.add_css_class("sol-name");
    lead.add_css_class("sol-display");
    lead.add_css_class("sol-ink");
    lead.set_halign(gtk::Align::Start);
    let soft = gtk::Label::new(Some("connect a journal"));
    soft.add_css_class("sol-faint");
    soft.set_halign(gtk::Align::Start);
    soft.set_wrap(true);
    status.append(&title);
    status.append(&lead);
    status.append(&soft);

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let sources = heading("sources");
    sources.set_hexpand(true);
    let so_far_head = gtk::Label::new(Some("so far"));
    so_far_head.add_css_class("sol-faint");
    let last_head = gtk::Label::new(Some("last taken in"));
    last_head.add_css_class("sol-faint");
    last_head.set_width_chars(14);
    last_head.set_xalign(1.0);
    head.append(&sources);
    head.append(&so_far_head);
    head.append(&last_head);
    status.append(&head);

    let rows = [
        (TileId::Screen, "screen", GLYPH_SCREEN),
        (TileId::Terminal, "terminal", GLYPH_TERMINAL),
        (TileId::Browsers, "browsers", GLYPH_BROWSERS),
        (TileId::Obsidian, "Obsidian", GLYPH_OBSIDIAN),
    ];
    let mut facts = Vec::new();
    for (_, name, glyph) in rows {
        let (row, so_far, last) = source_row(name, glyph);
        status.append(&row);
        facts.push(FactWidgets { so_far, last });
    }

    let journal_head = heading("your journal");
    status.append(&journal_head);
    let kept_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let kept_name = gtk::Label::new(Some("kept on this computer"));
    kept_name.add_css_class("sol-ink");
    kept_name.set_halign(gtk::Align::Start);
    kept_name.set_hexpand(true);
    let kept = gtk::Label::new(Some("nothing yet"));
    kept.add_css_class("sol-faint");
    kept.set_halign(gtk::Align::End);
    kept_row.append(&kept_name);
    kept_row.append(&kept);
    status.append(&kept_row);

    let details = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let details_name = gtk::Label::new(Some("technical details"));
    details_name.add_css_class("sol-ink");
    details_name.set_halign(gtk::Align::Start);
    details_name.set_hexpand(true);
    let chevron = gtk::Label::new(Some("›"));
    chevron.add_css_class("sol-faint");
    details.append(&details_name);
    details.append(&chevron);
    status.append(&details);

    let unread = gtk::Label::new(None);
    unread.add_css_class("sol-faint");
    unread.set_halign(gtk::Align::Start);
    unread.set_wrap(true);
    unread.set_visible(false);
    status.append(&unread);
    pane.append(&status);

    let page_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page_box.add_css_class("sol-page");
    page_box.set_visible(false);
    let page_top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let page_title = gtk::Label::new(None);
    page_title.add_css_class("sol-name");
    page_title.add_css_class("sol-display");
    page_title.add_css_class("sol-ink");
    page_title.set_halign(gtk::Align::Start);
    page_title.set_hexpand(true);
    let close = gtk::Button::with_label("×");
    close.set_tooltip_text(Some("close"));
    page_top.append(&page_title);
    page_top.append(&close);
    let word = gtk::Label::new(None);
    word.set_halign(gtk::Align::Start);
    let reason = gtk::Label::new(None);
    reason.add_css_class("sol-faint");
    reason.set_halign(gtk::Align::Start);
    reason.set_wrap(true);
    reason.set_visible(false);
    let so_far = gtk::Label::new(None);
    so_far.add_css_class("sol-faint");
    so_far.set_halign(gtk::Align::Start);
    let last = gtk::Label::new(None);
    last.add_css_class("sol-faint");
    last.set_halign(gtk::Align::Start);
    page_box.append(&page_top);
    page_box.append(&word);
    page_box.append(&reason);
    page_box.append(&so_far);
    page_box.append(&last);
    pane.append(&page_box);

    let facts = [
        facts.remove(0),
        facts.remove(0),
        facts.remove(0),
        facts.remove(0),
    ];
    BuiltPane {
        root: pane,
        facts,
        lead,
        soft,
        kept,
        unread,
        page: Page {
            status,
            box_: page_box,
            title: page_title,
            word,
            reason,
            close,
            so_far,
            last,
        },
    }
}

fn heading(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("sol-display");
    label.add_css_class("sol-faint");
    label.set_halign(gtk::Align::Start);
    label
}

fn source_row(name: &str, glyph: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&glyph_widget(glyph));
    let title = gtk::Label::new(Some(name));
    title.add_css_class("sol-ink");
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    title.set_xalign(0.0);
    let so_far = gtk::Label::new(Some("nothing yet"));
    so_far.add_css_class("sol-ink");
    so_far.set_halign(gtk::Align::End);
    so_far.set_xalign(1.0);
    let last = gtk::Label::new(None);
    last.add_css_class("sol-faint");
    last.set_halign(gtk::Align::End);
    last.set_xalign(1.0);
    last.set_width_chars(14);
    row.append(&title);
    row.append(&so_far);
    row.append(&last);
    (row, so_far, last)
}

fn fill_shelf(shelf: &gtk::Box) -> Vec<ShelfWidgets> {
    let rows = [
        ("home", ShelfRow::Home, GLYPH_HOME),
        ("your journal", ShelfRow::Journal, GLYPH_JOURNAL),
        ("this computer", ShelfRow::Computer, GLYPH_DEVICE),
        ("updates", ShelfRow::Updates, GLYPH_UPDATES),
        ("help", ShelfRow::Help, GLYPH_HELP),
    ];
    rows.into_iter()
        .map(|(label, which, glyph)| {
            let button = gtk::Button::new();
            button.add_css_class("sol-shelf-row");
            button.add_css_class("sol-ink");
            button.set_halign(gtk::Align::Fill);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
            row.set_hexpand(true);
            row.append(&glyph_widget(glyph));
            let text = gtk::Label::new(Some(label));
            text.add_css_class("sol-ink");
            text.set_halign(gtk::Align::Start);
            text.set_hexpand(true);
            text.set_xalign(0.0);
            let badge = mark_widget();
            badge.set_size_request(8, 8);
            badge.add_css_class("sol-on");
            badge.set_visible(false);
            badge.set_halign(gtk::Align::End);
            row.append(&text);
            row.append(&badge);
            button.set_child(Some(&row));
            shelf.append(&button);
            ShelfWidgets {
                button,
                which,
                badge: badge.upcast(),
            }
        })
        .collect()
}

fn pause_controls() -> (gtk::MenuButton, gtk::Button) {
    let pause = gtk::MenuButton::new();
    pause.set_label("pause");
    pause.add_css_class("sol-pill");
    let popover = gtk::Popover::new();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for (label, _) in shell_model::pause_menu() {
        menu.append(&gtk::Button::with_label(label));
    }
    popover.set_child(Some(&menu));
    pause.set_popover(Some(&popover));
    let resume = gtk::Button::with_label("resume");
    resume.add_css_class("sol-pill");
    resume.set_visible(false);
    (pause, resume)
}

fn status_pill() -> (gtk::Button, gtk::DrawingArea, gtk::Label) {
    let pill = gtk::Button::new();
    pill.add_css_class("sol-pill");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let mark = mark_widget();
    let label = gtk::Label::new(Some("not paired"));
    label.add_css_class("sol-ink");
    row.append(&mark);
    row.append(&label);
    pill.set_child(Some(&row));
    (pill, mark, label)
}

fn journal_pill() -> (gtk::Box, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::Center);
    row.add_css_class("sol-journal");
    let left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    left.add_css_class("sol-chip");
    left.set_size_request(11, 11);
    let right = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    right.add_css_class("sol-chip");
    right.set_size_request(11, 11);
    let label = gtk::Label::new(Some("your · journal"));
    label.add_css_class("sol-faint");
    row.append(&left);
    row.append(&right);
    row.append(&label);
    (row, label)
}

fn wire_pause(
    ui: &Rc<Ui>,
    view: &Rc<RefCell<View>>,
    seen: &Rc<Cell<bool>>,
    daemon: &Rc<Cell<Option<bool>>>,
    selected: &Rc<Cell<Option<TileId>>>,
) {
    let Some(popover) = ui.pause_button.popover() else {
        return;
    };
    let Some(menu) = popover
        .child()
        .and_then(|child| child.downcast::<gtk::Box>().ok())
    else {
        return;
    };
    let mut child = menu.first_child();
    for (label, duration) in shell_model::pause_menu() {
        let Some(widget) = child.take() else {
            break;
        };
        child = widget.next_sibling();
        let Some(button) = widget.downcast::<gtk::Button>().ok() else {
            continue;
        };
        if button.label().as_deref() != Some(label) {
            continue;
        }
        let view = Rc::clone(view);
        let seen = Rc::clone(seen);
        let daemon = Rc::clone(daemon);
        let ui = Rc::clone(ui);
        let selected = Rc::clone(selected);
        let popover = popover.clone();
        button.connect_clicked(move |_| {
            hold_pause(duration);
            tell_service_pause(duration);
            popover.popdown();
            refresh(&ui, &view, &seen, &daemon, &selected);
        });
    }
    let view = Rc::clone(view);
    let seen = Rc::clone(seen);
    let daemon = Rc::clone(daemon);
    let resume_ui = Rc::clone(ui);
    let selected = Rc::clone(selected);
    ui.resume_button.connect_clicked(move |_| {
        let _ = pause_hold::clear(&state_dir());
        glib::MainContext::default().spawn_local(async {
            if let Ok(client) = solstone_linux::dbus_client::ControlClient::session().await {
                let _ = client.resume().await;
            }
        });
        refresh(&resume_ui, &view, &seen, &daemon, &selected);
    });
}

fn hold_pause(duration: PauseFor) {
    let wall = unix_secs();
    let held = match duration {
        PauseFor::UntilResume => HeldPause::UntilResumed,
        PauseFor::Seconds(seconds) => HeldPause::Until(wall + f64::from(seconds)),
    };
    let _ = pause_hold::save(&state_dir(), held);
}

fn tell_service_pause(duration: PauseFor) {
    let seconds = match duration {
        PauseFor::UntilResume => 0,
        PauseFor::Seconds(seconds) => i32::try_from(seconds).unwrap_or(i32::MAX),
    };
    glib::MainContext::default().spawn_local(async move {
        if let Ok(client) = solstone_linux::dbus_client::ControlClient::session().await {
            let _ = client.pause(seconds).await;
        }
    });
}

fn wire_switch(
    switch: Option<&gtk::Switch>,
    which: OwnerSwitch,
    view: Rc<RefCell<View>>,
    seen: Rc<Cell<bool>>,
    daemon: Rc<Cell<Option<bool>>>,
    ui: Rc<Ui>,
    selected: Rc<Cell<Option<TileId>>>,
) {
    let Some(switch) = switch else {
        return;
    };
    switch.connect_state_set(move |_, on| {
        let paths = config::ConfigPaths::default();
        if config::set_owner_switch(&paths, which, on).is_err() {
            return glib::Propagation::Stop;
        }
        if which == OwnerSwitch::Terminal && on {
            daemon.set(None);
        }
        refresh(&ui, &view, &seen, &daemon, &selected);
        if which == OwnerSwitch::Terminal && on {
            probe_daemon(
                Rc::clone(&daemon),
                Rc::clone(&view),
                Rc::clone(&seen),
                Rc::clone(&ui),
                Rc::clone(&selected),
            );
        }
        glib::Propagation::Proceed
    });
}

fn probe_daemon(
    daemon: Rc<Cell<Option<bool>>>,
    view: Rc<RefCell<View>>,
    seen: Rc<Cell<bool>>,
    ui: Rc<Ui>,
    selected: Rc<Cell<Option<TileId>>>,
) {
    if !view.borrow().terminal_on {
        return;
    }
    let window = ui.window.clone();
    glib::MainContext::default().spawn_local(async move {
        let probed = gio::spawn_blocking(|| {
            solstone_linux::service::user_service_pid(&solstone_linux::cli::SystemRunner)
                .ok()
                .is_some_and(|pid| pid > 0)
        })
        .await
        .unwrap_or(false);
        daemon.set(Some(probed));
        if window.is_mapped() {
            refresh(&ui, &view, &seen, &daemon, &selected);
        }
    });
}

fn refresh(
    ui: &Ui,
    view: &Rc<RefCell<View>>,
    seen: &Rc<Cell<bool>>,
    daemon: &Rc<Cell<Option<bool>>>,
    selected: &Cell<Option<TileId>>,
) {
    let next = read_view(seen, daemon.get());
    apply_view(ui, &next);
    if let Some(id) = selected.get() {
        ui.page.show_tile(id, &next);
    }
    *view.borrow_mut() = next;
}

fn apply_view(ui: &Ui, view: &View) {
    ui.greet.set_text(shell_model::greeting_now());
    let host = HOST_UP.with(|cell| cell.get());
    let show_pause =
        view.pause.is_none() && shell_model::header_shows_pause(host == Some(true), host.is_none());
    match &view.pause {
        Some(left) => {
            ui.pause_line.set_text(&shell_model::pause_line(*left));
            ui.pause_line.set_visible(true);
            ui.pause_button.set_visible(false);
            ui.resume_button.set_visible(true);
        }
        None => {
            ui.pause_line.set_visible(false);
            ui.pause_button.set_visible(show_pause);
            ui.resume_button.set_visible(false);
        }
    }
    paint_pairing(ui, view);
    let words = [view.screen, view.terminal, view.browsers, view.obsidian];
    let subs = tile_subs(view);
    for (tile, (word, sub)) in ui.tiles.iter().zip(words.into_iter().zip(subs)) {
        tile.word.set_text(shell_model::state_label(word));
        paint_word(&tile.word, word);
        paint_mark(&tile.mark, word);
        match sub {
            Some(text) => {
                tile.sub.set_text(&text);
                tile.sub.set_visible(true);
            }
            None => tile.sub.set_visible(false),
        }
        if let Some(switch) = &tile.switch {
            let on = match tile.id {
                TileId::Screen => view.screen_on,
                TileId::Terminal => view.terminal_on,
                TileId::Browsers | TileId::Obsidian => false,
            };
            if switch.is_active() != on && !switch.has_focus() {
                switch.set_active(on);
            }
        }
    }
    let ids = [
        TileId::Screen,
        TileId::Terminal,
        TileId::Browsers,
        TileId::Obsidian,
    ];
    let mut hidden = false;
    for (fact, id) in ui.facts.iter().zip(ids) {
        let (so_far, last) = facts_for(id, &view.intake);
        match &so_far {
            Some(text) => fact.so_far.set_text(text),
            None => {
                fact.so_far.set_text("");
                hidden = true;
            }
        }
        match &last {
            Some(text) => fact.last.set_text(text),
            None => fact.last.set_text(""),
        }
    }
    ui.kept.set_text(&view.kept);
    if hidden {
        ui.unread.set_text(shell_model::INTAKE_UNREADABLE);
        ui.unread.set_visible(true);
    } else {
        ui.unread.set_visible(false);
    }
    for shelf in &ui.shelf_rows {
        let show = shelf.which == ShelfRow::Journal && view.offline_waiting.is_some();
        shelf.badge.set_visible(show);
    }
}

fn paint_word(label: &gtk::Label, word: StateWord) {
    if word == StateWord::NeedsAttention {
        label.add_css_class("sol-attn");
        label.remove_css_class("sol-faint");
    } else {
        label.add_css_class("sol-faint");
        label.remove_css_class("sol-attn");
    }
}

fn facts_for(id: TileId, file: &IntakeFile) -> (Option<String>, Option<String>) {
    let total = match id {
        TileId::Screen => shell_model::screen_so_far(file),
        TileId::Terminal => shell_model::terminal_so_far(file),
        TileId::Browsers => shell_model::browsers_so_far(file),
        TileId::Obsidian => shell_model::obsidian_so_far(file),
    };
    let last_raw = match file {
        IntakeFile::Rows(rows) => counter_last(rows, id),
        IntakeFile::Missing | IntakeFile::Refused => None,
    };
    let last = shell_model::source_last_now(file, last_raw);
    (
        shell_model::total_text(&total).map(str::to_owned),
        shell_model::last_text(&last).map(str::to_owned),
    )
}

fn counter_last(rows: &IntakeRows, id: TileId) -> Option<&str> {
    let counter = match id {
        TileId::Screen => &rows.screen,
        TileId::Terminal => &rows.terminal,
        TileId::Browsers => &rows.browsers,
        TileId::Obsidian => &rows.obsidian,
    };
    counter.last.as_deref()
}

fn set_fact(label: &gtk::Label, text: Option<&str>) {
    match text {
        Some(text) => {
            label.set_text(text);
            label.set_visible(true);
        }
        None => label.set_visible(false),
    }
}

fn apply_layout(
    ui: &Ui,
    selected: &Cell<Option<TileId>>,
    row: &Cell<ShelfRow>,
    shelf_open: &Cell<bool>,
    page_open: &Cell<bool>,
) {
    let width = ui.window.width().max(ui.window.default_width());
    let class = shell_model::width_class(width);
    let phone = class == WidthClass::Phone;
    let tuck = shell_model::shelf_button(width);
    if tuck {
        ui.shelf_button.set_visible(true);
        ui.shelf.set_visible(shelf_open.get());
        ui.body.set_margin_start(sun_bg::INSET);
    } else {
        ui.shelf_button.set_visible(false);
        ui.shelf.set_visible(true);
        ui.body
            .set_margin_start(sun_bg::INSET + sun_bg::SHELF_WIDTH + sun_bg::INSET);
    }
    let cols = i32::from(shell_model::deck_columns(width));
    place_tiles(&ui.grid, &ui.tiles, &ui.add_more, cols);
    let home = row.get() == ShelfRow::Home;
    ui.grid.set_visible(home);
    ui.other.set_visible(!home);
    if !home {
        ui.other.set_text(shelf_label(row.get()));
    }
    for shelf in &ui.shelf_rows {
        if shelf.which == row.get() {
            shelf.button.add_css_class("sol-selected");
        } else {
            shelf.button.remove_css_class("sol-selected");
        }
    }
    for tile in &ui.tiles {
        if selected.get() == Some(tile.id) {
            tile.root.add_css_class("sol-selected");
        } else {
            tile.root.remove_css_class("sol-selected");
        }
    }
    let pane_open = home && (!phone || page_open.get());
    if phone && home {
        ui.grid.set_visible(!page_open.get());
    } else if home {
        ui.grid.set_visible(true);
    }
    ui.pane.set_visible(pane_open && home);
    if pane_open && !phone {
        ui.pane.set_halign(gtk::Align::End);
        ui.pane.set_size_request(PANE_WIDTH, -1);
        ui.pane.set_margin_start(0);
        ui.pane.set_margin_top(0);
        ui.pane.set_margin_end(0);
        ui.pane.set_margin_bottom(0);
        ui.body.set_margin_end(PANE_WIDTH);
    } else if pane_open && phone {
        ui.pane.set_halign(gtk::Align::Fill);
        ui.pane.set_size_request(-1, -1);
        ui.pane.set_margin_start(ui.body.margin_start());
        ui.pane.set_margin_top(sun_bg::HEADER_HEIGHT);
        ui.pane.set_margin_end(0);
        ui.pane.set_margin_bottom(0);
        ui.body.set_margin_end(sun_bg::INSET);
    } else {
        ui.body.set_margin_end(sun_bg::INSET);
    }
}

fn place_tiles(grid: &gtk::Grid, tiles: &[TileWidgets], add_more: &gtk::Box, cols: i32) {
    let cols = cols.max(1);
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
    let mut index = 0i32;
    for tile in tiles {
        grid.attach(&tile.root, index % cols, index / cols, 1, 1);
        index += 1;
    }
    grid.attach(add_more, index % cols, index / cols, 1, 1);
}

fn shelf_label(row: ShelfRow) -> &'static str {
    match row {
        ShelfRow::Home => "home",
        ShelfRow::Journal => "your journal",
        ShelfRow::Computer => "this computer",
        ShelfRow::Updates => "updates",
        ShelfRow::Help => "help",
    }
}

fn shelf_button_mode(width: i32) -> bool {
    shell_model::shelf_button(width.max(1))
}

fn read_view(seen_report: &Cell<bool>, daemon_up: Option<bool>) -> View {
    let loaded = config::load_config(config::ConfigPaths::default());
    let config = loaded.config;
    let intake = source_intake::read_rows(&config.state_dir().join(source_intake::FILENAME));
    let paused = pause_left(&config);
    let app_paused = paused.is_some();
    let snap = current_snap();
    let activity_running = if snap.ready {
        snap.activity_source && !config.activity_only
    } else {
        config.activity_source && !config.activity_only
    };
    let screen = screen_now(&config, app_paused, activity_running, &snap);
    let (terminal, terminal_reason) = terminal_word(&config, daemon_up, app_paused);
    let (browsers, browsers_reason, browsers_sub) = browsers_word(&config, seen_report);
    let obsidian = obsidian_word(&snap);
    let (paired, offline, offline_waiting) = journal_face(&snap);
    View {
        screen_on: config.desktop_capture,
        terminal_on: config.terminal,
        screen: hold_pause_word(screen.word, app_paused),
        screen_sub: screen.sub,
        terminal: hold_pause_word(terminal, app_paused),
        terminal_reason,
        browsers: hold_pause_word(browsers, app_paused),
        browsers_reason,
        browsers_sub,
        obsidian: hold_pause_word(obsidian, app_paused),
        intake,
        pause: paused,
        paired,
        offline,
        offline_waiting,
        kept: snap.kept,
    }
}

fn current_snap() -> ServiceSnap {
    SERVICE.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|snap| snap.borrow().clone())
            .unwrap_or_else(ServiceSnap::unread)
    })
}

fn screen_now(
    config: &solstone_linux::config::Config,
    app_paused: bool,
    activity_running: bool,
    snap: &ServiceSnap,
) -> shell_model::ScreenTile {
    if config.activity_only || !config.desktop_capture {
        return shell_model::screen_tile(
            config.desktop_capture,
            config.activity_only,
            activity_running,
            TrayStatus::Idle,
        );
    }
    let status = if app_paused {
        Some(TrayStatus::Paused)
    } else if snap.ready {
        tray_from_capture(&snap.capture, snap.capture_attention)
    } else {
        None
    };
    let Some(status) = status else {
        return shell_model::ScreenTile {
            word: StateWord::Checking,
            switch_on: true,
            sub: ScreenSub::None,
        };
    };
    shell_model::screen_tile(
        config.desktop_capture,
        config.activity_only,
        activity_running,
        status,
    )
}

fn tray_from_capture(capture: &str, attention: bool) -> Option<TrayStatus> {
    match capture {
        "recording" => Some(TrayStatus::Recording),
        "idle" => Some(TrayStatus::Idle),
        "paused" => Some(TrayStatus::Paused),
        "stopped" => Some(TrayStatus::Stopped),
        "waiting" if attention => Some(TrayStatus::Waiting(SessionPresence::Unreachable)),
        "waiting" => Some(TrayStatus::Waiting(SessionPresence::NoDesktop)),
        _ => None,
    }
}

fn obsidian_word(snap: &ServiceSnap) -> StateWord {
    let Some(state) = snap.obsidian else {
        return StateWord::Checking;
    };
    shell_model::obsidian_tile(ObsidianFacts {
        state,
        vaults_found: snap.obsidian_vault,
        vault_chosen: snap.obsidian_vault,
    })
}

fn journal_face(snap: &ServiceSnap) -> (bool, bool, Option<i64>) {
    if !snap.ready {
        return (false, false, None);
    }
    let paired = snap.durable == "confirmed" || snap.durable == "held";
    if !paired {
        return (false, false, None);
    }
    let offline = snap.health == "offline";
    let waiting = if offline {
        snap.queue.filter(|count| *count > 0)
    } else {
        None
    };
    (true, offline, waiting)
}

fn hold_pause_word(word: StateWord, app_paused: bool) -> StateWord {
    if app_paused && word == StateWord::On {
        StateWord::Paused
    } else {
        word
    }
}

fn terminal_word(
    config: &solstone_linux::config::Config,
    daemon_up: Option<bool>,
    app_paused: bool,
) -> (StateWord, Option<&'static str>) {
    if !config.terminal {
        return (
            shell_model::terminal_tile(false, true, EngineState::Off, false, app_paused),
            None,
        );
    }
    let Some(up) = daemon_up else {
        return (StateWord::Checking, None);
    };
    if !up {
        return (
            StateWord::NeedsAttention,
            shell_model::terminal_daemon_reason(true, false),
        );
    }
    let Some(saved) = TerminalState::read(&config.state_dir()) else {
        return (StateWord::Checking, None);
    };
    (
        shell_model::terminal_tile(true, true, saved.state, saved.error.is_some(), app_paused),
        None,
    )
}

fn browsers_word(
    config: &solstone_linux::config::Config,
    seen_report: &Cell<bool>,
) -> (StateWord, Option<&'static str>, Option<String>) {
    let path = solstone_linux::browser::status::running_path(
        &solstone_linux::browser::custody_root(&config.base_dir),
    );
    let running = solstone_linux::browser::status::load_running(&path);
    if running.is_some() {
        seen_report.set(true);
    }
    let host = BrowserHost {
        updated_at_ms: running.as_ref().map(|status| status.updated_at_ms),
        now_ms: unix_ms(),
        seen_report: seen_report.get(),
        room_full: running.as_ref().is_some_and(|status| status.full),
        capture_paused: running
            .as_ref()
            .is_some_and(|status| status.capture == "paused"),
    };
    let names = running
        .as_ref()
        .map(|status| {
            status
                .browsers
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (
        shell_model::browsers_tile(host),
        shell_model::browser_host_reason(host),
        shell_model::browsers_subline(&names),
    )
}

fn pause_left(config: &solstone_linux::config::Config) -> Option<PauseLeft> {
    let wall = unix_secs();
    let starting = pause_hold::StartingPause::read(config, wall, wall);
    if !starting.paused {
        return None;
    }
    match starting.until_wall {
        Some(deadline) => {
            let left = (deadline - wall).ceil().max(0.0);
            Some(PauseLeft::Seconds(left as u32))
        }
        None => Some(PauseLeft::UntilResume),
    }
}

fn state_dir() -> std::path::PathBuf {
    config::load_config(config::ConfigPaths::default())
        .config
        .state_dir()
}

fn unix_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

const GLYPH_SCREEN: &str =
    r#"<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>"#;
const GLYPH_TERMINAL: &str = r#"<rect x="3" y="4.5" width="18" height="15" rx="2.5"/><path d="M7 9.5l3 2.5-3 2.5M12.5 15H17"/>"#;
const GLYPH_BROWSERS: &str = r#"<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c2.6 2.4 4 5.6 4 9s-1.4 6.6-4 9c-2.6-2.4-4-5.6-4-9s1.4-6.6 4-9z"/>"#;
const GLYPH_OBSIDIAN: &str =
    r#"<path d="M12 3l7 6-2.5 11h-9L5 9z"/><path d="M12 3v9l4.5 8M12 12l-7-3"/>"#;
const GLYPH_PLUS: &str = r#"<path d="M12 5v14M5 12h14"/>"#;
const GLYPH_HOME: &str = r#"<path d="M4 11l8-7 8 7v8a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 19z"/><path d="M10 20.5V14h4v6.5"/>"#;
const GLYPH_JOURNAL: &str = r#"<path d="M5 4.5A1.5 1.5 0 0 1 6.5 3H19v15H6.5A1.5 1.5 0 0 0 5 19.5z"/><path d="M5 19.5A1.5 1.5 0 0 0 6.5 21H19v-3"/>"#;
const GLYPH_DEVICE: &str =
    r#"<rect x="4" y="5" width="16" height="11" rx="1.5"/><path d="M2.5 19.5h19"/>"#;
const GLYPH_UPDATES: &str =
    r#"<path d="M12 4v10M8 10.5l4 4 4-4"/><path d="M4 12a8 8 0 1 0 16 0"/>"#;
const GLYPH_HELP: &str = r#"<circle cx="12" cy="12" r="9"/><path d="M9.5 9.5a2.5 2.5 0 1 1 3.6 2.3c-.8.4-1.1 1-1.1 1.7M12 17h.01"/>"#;

const ORANGE: (f64, f64, f64) = (232.0 / 255.0, 145.0 / 255.0, 58.0 / 255.0);
const KEPT_WALK_CAP: u32 = 10_000;
const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";

const DECK_CSS: &str = "\
.sol-display { font-family: \"Comfortaa\", sans-serif; font-weight: 700; }\
.sol-greet.sol-display { font-size: 22px; }\
.sol-name.sol-display { font-size: 17px; }\
.sol-tile {\
  background-color: #FEFCF8;\
  border-radius: 18px;\
  border: 1px solid rgba(58, 46, 36, 0.14);\
  padding: 12px 14px;\
  min-height: 112px;\
}\
window.sol-dark .sol-tile {\
  background-color: #2A221C;\
  border-color: rgba(255, 244, 230, 0.16);\
}\
.sol-tile.sol-selected { border-color: #E8913A; border-width: 2px; }\
.sol-tile.sol-dash {\
  background-color: transparent;\
  background-image: none;\
  border: 1.5px dashed #A99C86;\
}\
window.sol-dark .sol-tile.sol-dash {\
  background-color: transparent;\
  border-color: #A99C86;\
}\
.sol-pane {\
  background-color: #FEFCF8;\
  background-image: none;\
  border-radius: 0;\
  border-style: solid;\
  border-width: 0 0 0 1px;\
  border-color: rgba(58, 46, 36, 0.14);\
  box-shadow: none;\
  padding: 40px 20px 16px;\
}\
window.sol-dark .sol-pane {\
  background-color: #2A221C;\
  border-color: rgba(255, 244, 230, 0.16);\
}\
.sol-page {\
  background: none;\
  background-color: transparent;\
  border: none;\
  border-radius: 0;\
  box-shadow: none;\
  padding: 0;\
}\
window.sol-dark .sol-page {\
  background-color: transparent;\
  border-color: transparent;\
}\
button.sol-pill, menubutton.sol-pill {\
  background-color: #FEFCF8;\
  background-image: none;\
  border: 1px solid rgba(58, 46, 36, 0.16);\
  border-radius: 999px;\
  box-shadow: none;\
  padding: 6px 13px;\
  min-height: 0;\
  min-width: 0;\
  color: #3A2E24;\
}\
button.sol-pill:hover, button.sol-pill:active, button.sol-pill:checked,\
menubutton.sol-pill:hover, menubutton.sol-pill:active {\
  background-color: #FEFCF8;\
  background-image: none;\
  box-shadow: none;\
  color: #3A2E24;\
}\
window.sol-dark button.sol-pill, window.sol-dark menubutton.sol-pill,\
window.sol-dark button.sol-pill:hover, window.sol-dark menubutton.sol-pill:hover {\
  background-color: #2A221C;\
  color: #F6EDE3;\
  border-color: rgba(255, 244, 230, 0.22);\
}\
switch.sol-switch:checked {\
  background-color: #E8913A;\
  background-image: none;\
  border-color: #E8913A;\
}\
switch.sol-switch:checked slider {\
  background-color: #FEFCF8;\
  border: none;\
}\
.sol-tile:focus, .sol-pill:focus, .sol-shelf-row:focus, switch.sol-switch:focus {\
  outline: 2px solid #E8913A;\
  outline-offset: 2px;\
}\
.sol-shelf-row { padding: 7px 9px; }\
.sol-chip {\
  border-radius: 2px;\
  border: 1.5px dashed #A99C86;\
  min-width: 11px;\
  min-height: 11px;\
  background: none;\
}\
.sol-journal.sol-paired .sol-chip {\
  border-style: solid;\
  background-color: #E8913A;\
  border-color: #E8913A;\
}\
";

fn install_deck_css(window: &gtk::ApplicationWindow) {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(DECK_CSS);
    gtk::style_context_add_provider_for_display(
        &gtk::prelude::WidgetExt::display(window),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
}

fn place_pane(body: &gtk::Box, pane: &gtk::Box) {
    let Some(parent) = body.parent() else {
        return;
    };
    let Ok(overlay) = parent.downcast::<gtk::Overlay>() else {
        return;
    };
    let mut header = None;
    let mut shelf = None;
    let mut child = overlay.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.has_css_class("sol-chrome-header") {
            header = Some(widget);
        } else if widget.has_css_class("sol-shelf") {
            shelf = Some(widget);
        }
    }
    if let Some(header) = &header {
        overlay.remove_overlay(header);
    }
    if let Some(shelf) = &shelf {
        overlay.remove_overlay(shelf);
    }
    pane.set_can_focus(false);
    pane.set_hexpand(false);
    pane.set_vexpand(true);
    pane.set_halign(gtk::Align::End);
    pane.set_valign(gtk::Align::Fill);
    overlay.add_overlay(pane);
    if let Some(header) = &header {
        overlay.add_overlay(header);
    }
    if let Some(shelf) = &shelf {
        overlay.add_overlay(shelf);
    }
}

fn glyph_widget(paths: &str) -> gtk::Widget {
    let area = gtk::DrawingArea::new();
    area.set_size_request(24, 24);
    area.set_content_width(24);
    area.set_content_height(24);
    area.set_halign(gtk::Align::Start);
    area.set_valign(gtk::Align::Center);
    area.set_can_focus(false);
    let paths = paths.to_owned();
    area.set_draw_func(move |area, cr, width, height| {
        let px = width.min(height);
        if px < 2 {
            return;
        }
        let Some(surface) = raster_glyph(&paths, widget_dark(area.upcast_ref::<gtk::Widget>()), px)
        else {
            return;
        };
        let left = f64::from(width - px) / 2.0;
        let top = f64::from(height - px) / 2.0;
        if cr.set_source_surface(&surface, left, top).is_err() {
            return;
        }
        let _ = cr.paint();
    });
    area.upcast()
}

fn raster_glyph(paths: &str, dark: bool, px: i32) -> Option<gtk::cairo::ImageSurface> {
    let ink = if dark { "#F6EDE3" } else { "#3A2E24" };
    let document = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{px}\" height=\"{px}\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"{ink}\" stroke-width=\"1.7\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{paths}</svg>"
    );
    let tree = resvg::usvg::Tree::from_str(&document, &resvg::usvg::Options::default()).ok()?;
    let side = u32::try_from(px).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(side, side)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let mut data = pixmap.data().to_vec();
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let stride = px * 4;
    gtk::cairo::ImageSurface::create_for_data(data, gtk::cairo::Format::ARgb32, px, px, stride).ok()
}

#[derive(Clone, Copy)]
enum MarkKind {
    Dot,
    Ring,
    Diamond,
    Bars,
}

fn mark_widget() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_size_request(12, 12);
    area.set_content_width(12);
    area.set_content_height(12);
    area.set_halign(gtk::Align::Start);
    area.set_valign(gtk::Align::Center);
    area.set_can_focus(false);
    area.set_draw_func(draw_mark);
    area
}

fn paint_mark(area: &gtk::DrawingArea, word: StateWord) {
    let kind = match word {
        StateWord::On => MarkKind::Dot,
        StateWord::NeedsAttention => MarkKind::Diamond,
        StateWord::Paused => MarkKind::Bars,
        StateWord::Off | StateWord::Checking | StateWord::SettingUp | StateWord::ReadyToSetUp => {
            MarkKind::Ring
        }
    };
    apply_mark_kind(area, kind);
}

fn apply_mark_kind(area: &gtk::DrawingArea, kind: MarkKind) {
    for class in [
        "sol-mark-on",
        "sol-mark-ring",
        "sol-mark-attn",
        "sol-mark-paused",
    ] {
        area.remove_css_class(class);
    }
    let class = match kind {
        MarkKind::Dot => "sol-mark-on",
        MarkKind::Ring => "sol-mark-ring",
        MarkKind::Diamond => "sol-mark-attn",
        MarkKind::Bars => "sol-mark-paused",
    };
    area.add_css_class(class);
    area.queue_draw();
}

fn draw_mark(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, width: i32, height: i32) {
    let kind = if area.has_css_class("sol-mark-attn") {
        MarkKind::Diamond
    } else if area.has_css_class("sol-mark-paused") {
        MarkKind::Bars
    } else if area.has_css_class("sol-mark-on") || area.has_css_class("sol-on") {
        MarkKind::Dot
    } else {
        MarkKind::Ring
    };
    let dark = widget_dark(area.upcast_ref::<gtk::Widget>());
    let side = f64::from(width.min(height));
    if side < 2.0 {
        return;
    }
    let cx = f64::from(width) / 2.0;
    let cy = f64::from(height) / 2.0;
    let radius = side * 0.32;
    match kind {
        MarkKind::Dot => {
            cr.set_source_rgb(ORANGE.0, ORANGE.1, ORANGE.2);
            cr.arc(cx, cy, radius, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        }
        MarkKind::Ring => {
            let (red, green, blue) = ink_rgb(dark);
            cr.set_source_rgb(red, green, blue);
            cr.set_line_width(1.4);
            cr.arc(cx, cy, radius, 0.0, std::f64::consts::TAU);
            let _ = cr.stroke();
        }
        MarkKind::Diamond => {
            let (red, green, blue) = if dark {
                (240.0 / 255.0, 180.0 / 255.0, 168.0 / 255.0)
            } else {
                (159.0 / 255.0, 45.0 / 255.0, 45.0 / 255.0)
            };
            cr.set_source_rgb(red, green, blue);
            cr.move_to(cx, cy - radius);
            cr.line_to(cx + radius, cy);
            cr.line_to(cx, cy + radius);
            cr.line_to(cx - radius, cy);
            cr.close_path();
            let _ = cr.fill();
        }
        MarkKind::Bars => {
            let (red, green, blue) = ink_rgb(dark);
            cr.set_source_rgb(red, green, blue);
            let bar = side * 0.14;
            let gap = side * 0.10;
            let tall = radius * 1.7;
            let span = bar * 3.0 + gap * 2.0;
            let left = cx - span / 2.0;
            let top = cy - tall / 2.0;
            for index in 0..3 {
                let x = left + f64::from(index) * (bar + gap);
                cr.rectangle(x, top, bar, tall);
            }
            let _ = cr.fill();
        }
    }
}

fn ink_rgb(dark: bool) -> (f64, f64, f64) {
    if dark {
        (246.0 / 255.0, 237.0 / 255.0, 227.0 / 255.0)
    } else {
        (58.0 / 255.0, 46.0 / 255.0, 36.0 / 255.0)
    }
}

fn widget_dark(widget: &gtk::Widget) -> bool {
    widget
        .ancestor(gtk::Window::static_type())
        .is_some_and(|window| window.has_css_class("sol-dark"))
}

fn paint_pairing(ui: &Ui, view: &View) {
    if !view.paired {
        ui.pill_label.set_text("not paired");
        ui.lead.set_text("not paired");
        show_line(&ui.soft, Some("connect a journal"));
        apply_mark_kind(&ui.pill_mark, MarkKind::Ring);
        ui.journal.remove_css_class("sol-paired");
        ui.journal_label.set_text("your · journal");
        return;
    }
    ui.journal.add_css_class("sol-paired");
    ui.journal_label.set_text("your · journal");
    if view.offline {
        if let Some(count) = view.offline_waiting {
            let line = format!("offline · {count} waiting");
            ui.pill_label.set_text(&line);
            ui.lead.set_text(&line);
            show_line(&ui.soft, None);
            apply_mark_kind(&ui.pill_mark, MarkKind::Ring);
        } else {
            ui.pill_label.set_text("");
            ui.lead.set_text("");
            show_line(&ui.soft, None);
            apply_mark_kind(&ui.pill_mark, MarkKind::Ring);
        }
        return;
    }
    ui.pill_label.set_text("connected");
    ui.lead.set_text("all caught up");
    show_line(&ui.soft, Some("on this device"));
    apply_mark_kind(&ui.pill_mark, MarkKind::Dot);
}

fn show_line(label: &gtk::Label, text: Option<&str>) {
    match text {
        Some(text) => {
            label.set_text(text);
            label.set_visible(true);
        }
        None => {
            label.set_text("");
            label.set_visible(false);
        }
    }
}

fn tile_subs(view: &View) -> [Option<String>; 4] {
    let off_when_unpaired =
        |word: StateWord| (word == StateWord::Off && !view.paired).then(|| OFF_SUB.to_owned());
    let screen = shell_model::screen_sub_label(view.screen_sub)
        .map(str::to_owned)
        .or_else(|| off_when_unpaired(view.screen));
    let terminal = view
        .terminal_reason
        .map(str::to_owned)
        .or_else(|| off_when_unpaired(view.terminal));
    let browsers = view
        .browsers_reason
        .map(str::to_owned)
        .or_else(|| view.browsers_sub.clone())
        .or_else(|| off_when_unpaired(view.browsers));
    let obsidian = off_when_unpaired(view.obsidian);
    [screen, terminal, browsers, obsidian]
}

fn watch_service(
    snap: Rc<RefCell<ServiceSnap>>,
    view: Rc<RefCell<View>>,
    seen: Rc<Cell<bool>>,
    daemon: Rc<Cell<Option<bool>>>,
    ui: Rc<Ui>,
    selected: Rc<Cell<Option<TileId>>>,
) {
    let frame = std::env::var("SOLSTONE_FRAME_PNG").is_ok_and(|path| !path.is_empty());
    let released = Rc::new(Cell::new(false));
    if frame {
        sun_bg::set_frame_hold(true);
        let released = Rc::clone(&released);
        let window = ui.window.clone();
        glib::timeout_add_local_once(Duration::from_secs(8), move || {
            if released.get() {
                return;
            }
            released.set(true);
            sun_bg::set_frame_hold(false);
            sun_bg::try_frame_file(&window);
        });
    }
    glib::MainContext::default().spawn_local(async move {
        loop {
            let host = watcher_present().await;
            HOST_UP.with(|cell| cell.set(host));
            if let Some(mut next) = fetch_service().await {
                next.kept = kept_label().await;
                *snap.borrow_mut() = next;
                refresh(&ui, &view, &seen, &daemon, &selected);
                if frame && !released.get() {
                    released.set(true);
                    let window = ui.window.clone();
                    glib::idle_add_local_once(move || {
                        sun_bg::set_frame_hold(false);
                        sun_bg::try_frame_file(&window);
                    });
                }
            } else if ui.window.is_mapped() {
                refresh(&ui, &view, &seen, &daemon, &selected);
            }
            glib::timeout_future(Duration::from_secs(2)).await;
        }
    });
}

fn apply_document(text: &str, obsidian_text: Option<&str>) -> Option<ServiceSnap> {
    let doc: solstone_linux::window_control::ControlDocument = serde_json::from_str(text).ok()?;
    let decoded = obsidian_text.and_then(solstone_linux::obsidian::decode_document);
    Some(ServiceSnap {
        ready: true,
        capture: doc.capture,
        capture_attention: doc.capture_attention,
        activity_source: doc.settings.activity_source,
        durable: doc.durable,
        health: doc.health,
        queue: doc.queue_count,
        obsidian: decoded.as_ref().map(|document| document.status.state),
        obsidian_vault: decoded
            .as_ref()
            .is_some_and(|document| document.status.vault.is_some()),
        kept: "nothing yet".to_owned(),
    })
}

async fn fetch_service() -> Option<ServiceSnap> {
    let client = solstone_linux::dbus_client::ControlClient::session()
        .await
        .ok()?;
    let state = client.get_control_state().await.ok()?;
    let obsidian = client.obsidian_state().await.ok();
    apply_document(&state, obsidian.as_deref())
}

async fn watcher_present() -> Option<bool> {
    let bus = gio::bus_get_future(gio::BusType::Session).await.ok()?;
    let reply = bus
        .call_future(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&glib::Variant::from((WATCHER_NAME.to_owned(),))),
            None,
            gio::DBusCallFlags::NONE,
            800,
        )
        .await
        .ok()?;
    reply.child_value(0).get::<bool>()
}

async fn kept_label() -> String {
    gio::spawn_blocking(measure_kept)
        .await
        .unwrap_or_else(|_| "nothing yet".to_owned())
}

fn measure_kept() -> String {
    let config = config::load_config(config::ConfigPaths::default()).config;
    let mut bytes = 0u64;
    let mut seen = 0u32;
    let mut stack = vec![config.captures_dir(), config.state_dir()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen = seen.saturating_add(1);
            if seen > KEPT_WALK_CAP {
                return format_kept(bytes);
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if kind.is_file()
                && let Ok(meta) = entry.metadata()
            {
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }
    format_kept(bytes)
}

fn format_kept(bytes: u64) -> String {
    if bytes == 0 {
        return "nothing yet".to_owned();
    }
    const MB: u64 = 1_000_000;
    const KB: u64 = 1_000;
    if bytes < MB {
        return format!("{} KB", (bytes / KB).max(1));
    }
    format!("{} MB", bytes / MB)
}
