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
use solstone_linux::window_control::MarkRenderDocument;

use crate::sun_bg::{self, Chrome};

const PANE_WIDTH: i32 = 360;
/// Space between the deck and the pane's hairline. The shelf inset stays 8 px.
const PANE_GUTTER: i32 = 24;
const OFF_SUB: &str = "turn it on any time.";
const SCREEN_WAITING: &str = "waiting to take in the screen.";
const SCREEN_BREATH: &str = "while this is on, the solstone app takes in what's on your screen, the sound, and which apps and windows you're using, and all of it goes into your journal.";
const SCREEN_SOUND: &str = "sound covers your microphone and what this computer plays. if there is no microphone, neither comes in.";
const TERMINAL_BREATH: &str = "the solstone app takes in your tmux sessions, and all of it goes into your journal. needs tmux on this computer, and works in any terminal session, including when you SSH into this computer.";
const BROWSERS_BREATH: &str = "on the sites you allow, the solstone extension takes in the page's text and rough layout, and all of it goes into your journal.";
const BROWSERS_GAP: &str =
    "pages come in from the solstone extension. there is no switch for it here.";
const GLYPH_SIDE: &str =
    r#"<rect x="3" y="5" width="18" height="14" rx="2.5"/><path d="M9 5v14"/>"#;

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
    /// The frame fixture names a journal. A live reading does not.
    fixture_mark: bool,
    /// The paired journal's mark, as the running app reports it.
    mark: Option<MarkRenderDocument>,
    /// Paired, and the service health is offline. A waiting count is separate.
    offline: bool,
    offline_waiting: Option<i64>,
    connection: Option<String>,
    last_reached: Option<String>,
    kept: String,
    activity_source: bool,
    activity_titles: bool,
    activity_urls: bool,
    activity_through_pause: bool,
    activity_only: bool,
    exclude_apps: usize,
    exclude_titles: usize,
    draw_cursor: bool,
    start_paused: bool,
    segment_interval: i64,
}

#[derive(Clone)]
struct ServiceSnap {
    ready: bool,
    capture: String,
    capture_attention: bool,
    activity_source: bool,
    activity_through_pause: bool,
    durable: String,
    health: String,
    last_sync: Option<f64>,
    queue: Option<i64>,
    obsidian: Option<solstone_linux::obsidian::State>,
    obsidian_vault: bool,
    kept: String,
    /// The running app's own pause: `Some(bounded)` while it is paused.
    pause_bounded: Option<bool>,
    mark: Option<MarkRenderDocument>,
}

impl ServiceSnap {
    fn unread() -> Self {
        Self {
            ready: false,
            capture: String::new(),
            capture_attention: false,
            activity_source: true,
            activity_through_pause: false,
            durable: String::new(),
            health: String::new(),
            last_sync: None,
            queue: None,
            obsidian: None,
            obsidian_vault: false,
            kept: "nothing yet".to_owned(),
            pause_bounded: None,
            mark: None,
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
    // The paused greeting is two lines. The pills keep their own height beside it.
    for widget in [
        pause_button.upcast_ref::<gtk::Widget>(),
        resume_button.upcast_ref(),
        pill.upcast_ref(),
    ] {
        widget.set_valign(gtk::Align::Center);
    }
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
    let (journal, journal_label, journal_mark) = journal_pill();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.add_css_class("sol-deck-column");
    column.set_hexpand(true);
    column.set_vexpand(true);
    let deck_scroll = gtk::ScrolledWindow::new();
    deck_scroll.add_css_class("sol-deck-scroll");
    deck_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    deck_scroll.set_propagate_natural_height(false);
    deck_scroll.set_vexpand(true);
    deck_scroll.set_child(Some(&grid));
    column.append(&greet_row);
    column.append(&deck_scroll);
    column.append(&journal);

    let other = gtk::Label::new(None);
    other.add_css_class("sol-name");
    other.add_css_class("sol-display");
    other.add_css_class("sol-ink");
    other.set_halign(gtk::Align::Start);
    other.set_valign(gtk::Align::Start);
    // The journal and help pages are built later, with the rest of the window.
    // The journal lands under the row's name. Help lands on its own card, which
    // carries its title.
    let journal_slot = page_slot();
    let help_slot = page_slot();
    help_slot.add_css_class("sol-pane");
    help_slot.add_css_class("sol-flush");
    let other_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    other_box.set_hexpand(true);
    other_box.set_vexpand(true);
    other_box.append(&other);
    other_box.append(&journal_slot);
    other_box.set_visible(false);
    SLOTS.with(|slot| *slot.borrow_mut() = Some((journal_slot.clone(), help_slot.clone())));

    let shelf_button = gtk::Button::new();
    shelf_button.add_css_class("sol-glyph");
    shelf_button.set_has_frame(false);
    shelf_button.set_tooltip_text(Some("sidebar"));
    shelf_button.set_child(Some(&glyph_widget(GLYPH_SIDE)));
    shelf_button.set_visible(false);
    let (computer, cursor_switch, paused_switch, segment_value, computer_note) = computer_page();
    let (updates, updates_journal_row, updates_journal) = updates_page();

    chrome.body.append(&column);
    chrome.body.append(&other_box);
    chrome.body.append(&computer);
    chrome.body.append(&updates);
    chrome.body.append(&help_slot);
    // The shelf glyph sits on the window, not in this slot.
    chrome.header_extra.set_visible(false);
    place_shelf_button(&chrome.body, &shelf_button);

    let shelf_rows = fill_shelf(&chrome.shelf);

    let ui = Rc::new(Ui {
        window: window.clone(),
        shelf: chrome.shelf.clone(),
        body: chrome.body.clone(),
        grid: grid.clone(),
        other: other.clone(),
        other_box,
        journal_slot,
        help_slot,
        computer: computer.clone(),
        updates,
        updates_journal_row,
        updates_journal,
        cursor: cursor_switch,
        start_paused: paused_switch,
        computer_note,
        computer_painting: Cell::new(false),
        tile_painting: Cell::new(false),
        segment: segment_value,
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
        connect: built.connect,
        soft: built.soft,
        kept_row: built.kept_row,
        kept: built.kept,
        connection_row: built.connection_row,
        connection: built.connection,
        reached_row: built.reached_row,
        reached: built.reached,
        unread: built.unread,
        page: built.page,
        shelf_rows,
        journal,
        journal_label,
        journal_mark,
        journal_mark_shown: RefCell::new(None),
    });

    for tile in &ui.tiles {
        let id = tile.id;
        let ui = Rc::clone(&ui);
        let selected = Rc::clone(&selected);
        let page_open = Rc::clone(&page_open);
        let row = Rc::clone(&row);
        let shelf_open = Rc::clone(&shelf_open);
        let view = Rc::clone(&view);
        let open: Rc<dyn Fn()> = Rc::new(move || {
            row.set(ShelfRow::Home);
            selected.set(Some(id));
            page_open.set(true);
            ui.page.show_tile(id, &view.borrow());
            apply_layout(&ui, &selected, &row, &shelf_open, &page_open);
        });
        let click = gtk::GestureClick::new();
        let open_click = Rc::clone(&open);
        click.connect_released(move |_, _, _, _| open_click());
        tile.root.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if matches!(
                key,
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::space
            ) {
                open();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        tile.root.add_controller(keys);
    }

    {
        let ui_back = Rc::clone(&ui);
        let selected_back = Rc::clone(&selected);
        let page_open = Rc::clone(&page_open);
        let row = Rc::clone(&row);
        let shelf_open = Rc::clone(&shelf_open);
        let back: Rc<dyn Fn()> = Rc::new(move || {
            row.set(ShelfRow::Home);
            selected_back.set(None);
            page_open.set(false);
            ui_back.page.show_status();
            apply_layout(&ui_back, &selected_back, &row, &shelf_open, &page_open);
        });
        let back_pill = Rc::clone(&back);
        pill.connect_clicked(move |_| back_pill());
        let back_close = Rc::clone(&back);
        let close = ui.page.close.clone();
        close.connect_clicked(move |_| back_close());
        let back_home = Rc::clone(&back);
        let home = ui.page.home.clone();
        home.connect_clicked(move |_| back_home());
        wire_page_switch(&ui, &view, &seen_report, &daemon_up, &selected);
        SHOW_HOME.with(|slot| *slot.borrow_mut() = Some(back));
    }

    let show_row: Rc<dyn Fn(ShelfRow)> = {
        let ui = Rc::clone(&ui);
        let row = Rc::clone(&row);
        let selected = Rc::clone(&selected);
        let page_open = Rc::clone(&page_open);
        let shelf_open = Rc::clone(&shelf_open);
        Rc::new(move |which| {
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
        })
    };
    for shelf in &ui.shelf_rows {
        let which = shelf.which;
        let show_row = Rc::clone(&show_row);
        shelf.button.connect_clicked(move |_| show_row(which));
    }
    {
        let show_row = Rc::clone(&show_row);
        ui.connect
            .connect_clicked(move |_| show_row(ShelfRow::Journal));
    }
    SHOW_JOURNAL.with(|slot| {
        *slot.borrow_mut() = Some(Rc::new(move || show_row(ShelfRow::Journal)));
    });

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
    wire_activity_save(&ui, &view, &seen_report, &daemon_up, &selected);
    wire_computer(&ui, &view, &seen_report, &daemon_up, &selected);

    apply_view(&ui, &view.borrow());
    apply_shot_hooks(&ui, &view, &selected, &row, &shelf_open, &page_open);
    apply_layout(&ui, &selected, &row, &shelf_open, &page_open);
    let focus_window = window.clone();
    focus_window.connect_map(move |window| {
        window.set_focus_visible(false);
        let none: Option<&gtk::Widget> = None;
        gtk::prelude::GtkWindowExt::set_focus(window, none);
    });

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
    static SHOW_JOURNAL: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    static SLOTS: RefCell<Option<(gtk::Box, gtk::Box)>> = const { RefCell::new(None) };
    /// `quit solstone` on help, shown while no panel menu carries it.
    static QUIT_ROW: RefCell<Option<gtk::Widget>> = const { RefCell::new(None) };
}

/// Open the journal row, where pairing lives.
pub fn show_journal() {
    SHOW_JOURNAL.with(|slot| {
        if let Some(show) = slot.borrow().as_ref() {
            show();
        }
    });
}

/// Put the journal and help pages under their shelf rows. `quit` is help's quit
/// row: it shows only while no panel menu carries `quit solstone`.
pub fn adopt_pages(
    journal: &impl IsA<gtk::Widget>,
    help: &impl IsA<gtk::Widget>,
    quit: &impl IsA<gtk::Widget>,
) {
    SLOTS.with(|slot| {
        if let Some((journal_slot, help_slot)) = slot.borrow().as_ref() {
            journal.set_vexpand(true);
            help.set_vexpand(true);
            journal_slot.append(journal);
            help_slot.append(help);
        }
    });
    quit.set_visible(quit_on_help());
    QUIT_ROW.with(|slot| *slot.borrow_mut() = Some(quit.as_ref().clone().upcast()));
}

/// The window carries quit while the panel menu is not up, the rule the header's
/// `pause` follows.
fn quit_on_help() -> bool {
    let host = HOST_UP.with(|cell| cell.get());
    shell_model::header_shows_pause(host == Some(true), host.is_none())
}

fn page_slot() -> gtk::Box {
    let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
    slot.add_css_class("sol-ink");
    slot.set_hexpand(true);
    slot.set_vexpand(true);
    slot.set_visible(false);
    slot
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
    other_box: gtk::Box,
    journal_slot: gtk::Box,
    help_slot: gtk::Box,
    computer: gtk::Box,
    updates: gtk::Box,
    updates_journal_row: gtk::Box,
    updates_journal: gtk::Label,
    cursor: gtk::Switch,
    start_paused: gtk::Switch,
    computer_note: gtk::Label,
    computer_painting: Cell<bool>,
    tile_painting: Cell<bool>,
    segment: gtk::Label,
    pane: gtk::Box,
    greet: gtk::Label,
    pause_line: gtk::Label,
    pill_mark: gtk::DrawingArea,
    pill_label: gtk::Label,
    pause_button: gtk::MenuButton,
    resume_button: gtk::Button,
    connect: gtk::Button,
    shelf_button: gtk::Button,
    tiles: Vec<TileWidgets>,
    add_more: gtk::Box,
    facts: [FactWidgets; 4],
    lead: gtk::Label,
    soft: gtk::Label,
    kept_row: gtk::Box,
    kept: gtk::Label,
    connection_row: gtk::Box,
    connection: gtk::Label,
    reached_row: gtk::Box,
    reached: gtk::Label,
    unread: gtk::Label,
    page: Page,
    shelf_rows: Vec<ShelfWidgets>,
    journal: gtk::Box,
    journal_label: gtk::Label,
    journal_mark: gtk::DrawingArea,
    /// The mark the pill is drawing now, so a repaint redraws only a new one.
    journal_mark_shown: RefCell<Option<MarkRenderDocument>>,
}

struct ShelfWidgets {
    button: gtk::Button,
    which: ShelfRow,
    badge: gtk::Widget,
}

struct Page {
    status: gtk::Box,
    box_: gtk::ScrolledWindow,
    home: gtk::Button,
    glyphs: [gtk::Widget; 4],
    title: gtk::Label,
    mark: gtk::DrawingArea,
    word: gtk::Label,
    switch: gtk::Switch,
    reason: gtk::Label,
    next: gtk::Label,
    breath: gtk::Label,
    sound: gtk::Label,
    so_far_row: gtk::Box,
    so_far: gtk::Label,
    last_row: gtk::Box,
    last: gtk::Label,
    screen_extra: gtk::Box,
    browsers_extra: gtk::Box,
    obsidian_extra: gtk::Box,
    obsidian_turn_off: gtk::Button,
    browsers_names: gtk::Label,
    activity: gtk::Switch,
    titles: gtk::Switch,
    urls: gtk::Switch,
    through_pause: gtk::Switch,
    activity_only: gtk::Switch,
    exclude_apps: gtk::Label,
    exclude_titles: gtk::Label,
    save: gtk::Button,
    save_note: gtk::Label,
    /// The owner changed a "goes along with it" switch that is not saved yet.
    edited: Rc<Cell<bool>>,
    close: gtk::Button,
    painting: Rc<Cell<bool>>,
}

impl Page {
    fn show_status(&self) {
        self.box_.set_visible(false);
        self.status.set_visible(true);
    }

    fn show_tile(&self, id: TileId, view: &View) {
        let word = match id {
            TileId::Screen => view.screen,
            TileId::Terminal => view.terminal,
            TileId::Browsers => view.browsers,
            TileId::Obsidian => view.obsidian,
        };
        let shown = match id {
            TileId::Screen => 0,
            TileId::Terminal => 1,
            TileId::Browsers => 2,
            TileId::Obsidian => 3,
        };
        for (index, glyph) in self.glyphs.iter().enumerate() {
            glyph.set_visible(index == shown);
        }
        self.title.set_text(shell_model::tile_name(id));
        self.word.set_text(shell_model::state_label(word));
        paint_word(&self.word, word);
        paint_mark(&self.mark, word);
        let (reason, next) = page_reason(id, word, view);
        set_fact(&self.reason, reason);
        set_fact(&self.next, next);
        self.breath.set_text(page_breath(id));
        let screen = id == TileId::Screen;
        self.sound.set_visible(screen);
        self.screen_extra.set_visible(screen);
        self.browsers_extra.set_visible(id == TileId::Browsers);
        self.obsidian_extra.set_visible(id == TileId::Obsidian);
        self.obsidian_turn_off.set_visible(matches!(
            view.obsidian,
            StateWord::On | StateWord::Paused | StateWord::NeedsAttention
        ));
        if let Some(names) = view.browsers_sub.as_deref() {
            self.browsers_names.set_text(names);
            self.browsers_names.set_visible(true);
        } else {
            self.browsers_names.set_visible(false);
        }
        if let Some(row) = self.browsers_names.parent() {
            row.set_visible(view.browsers_sub.is_some());
        }
        let (so_far, last) = page_facts(id, &view.intake);
        set_fact(&self.so_far, so_far.as_deref());
        self.so_far_row.set_visible(so_far.is_some());
        set_fact(&self.last, last.as_deref());
        self.last_row.set_visible(last.is_some());
        let show_switch = id != TileId::Browsers;
        self.switch.set_visible(show_switch);
        if show_switch {
            let on = match id {
                TileId::Screen => view.screen_on,
                TileId::Terminal => view.terminal_on,
                TileId::Obsidian => {
                    matches!(
                        word,
                        StateWord::On | StateWord::Paused | StateWord::NeedsAttention
                    )
                }
                TileId::Browsers => false,
            };
            self.painting.set(true);
            self.switch.set_active(on);
            self.painting.set(false);
        }
        // An unsaved change stays as the owner set it until they save.
        if !self.edited.get() {
            self.painting.set(true);
            self.activity.set_active(view.activity_source);
            self.titles.set_active(view.activity_titles);
            self.urls.set_active(view.activity_urls);
            self.through_pause.set_active(view.activity_through_pause);
            self.activity_only.set_active(view.activity_only);
            self.painting.set(false);
        }
        self.exclude_apps.set_text(&count_label(view.exclude_apps));
        self.exclude_titles
            .set_text(&count_label(view.exclude_titles));
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
    let root = if dashed {
        gtk::Box::new(gtk::Orientation::Vertical, 6)
    } else {
        // A source tile opens its page, so keyboard and assistive tech reach it
        // as a button.
        gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .accessible_role(gtk::AccessibleRole::Button)
            .focusable(true)
            .build()
    };
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
    word.add_css_class("sol-state");
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
    connect: gtk::Button,
    soft: gtk::Label,
    kept_row: gtk::Box,
    kept: gtk::Label,
    connection_row: gtk::Box,
    connection: gtk::Label,
    reached_row: gtk::Box,
    reached: gtk::Label,
    unread: gtk::Label,
    page: Page,
}

fn make_pane() -> BuiltPane {
    let pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    pane.add_css_class("sol-pane");
    pane.set_overflow(gtk::Overflow::Hidden);
    pane.set_size_request(PANE_WIDTH, -1);

    let status = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let title = gtk::Label::new(Some("status"));
    title.add_css_class("sol-display");
    title.add_css_class("sol-pane-title");
    title.add_css_class("sol-ink");
    title.set_halign(gtk::Align::Start);
    let lead = gtk::Label::new(Some("not paired"));
    lead.add_css_class("sol-display");
    lead.add_css_class("sol-lead");
    lead.add_css_class("sol-ink");
    lead.set_halign(gtk::Align::Start);
    lead.set_wrap(true);
    lead.set_max_width_chars(24);
    lead.set_xalign(0.0);
    // Pairing stays on the journal page. This button does not open another flow.
    let connect = gtk::Button::with_label("connect a journal");
    connect.add_css_class("sol-action");
    connect.set_halign(gtk::Align::Start);
    let soft = gtk::Label::new(None);
    soft.add_css_class("sol-faint");
    soft.set_halign(gtk::Align::Start);
    soft.set_xalign(0.0);
    soft.set_wrap(true);
    soft.set_max_width_chars(32);
    soft.set_visible(false);
    status.append(&title);
    status.append(&lead);
    status.append(&connect);
    status.append(&soft);

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    head.add_css_class("sol-rule");
    let sources = section_label("sources");
    sources.set_hexpand(true);
    let so_far_head = gtk::Label::new(Some("so far"));
    so_far_head.add_css_class("sol-section");
    so_far_head.add_css_class("sol-faint");
    let last_head = gtk::Label::new(Some("last taken in"));
    last_head.add_css_class("sol-section");
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

    let journal_head = section_label("your journal");
    journal_head.add_css_class("sol-rule");
    status.append(&journal_head);
    let kept_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    kept_row.add_css_class("sol-row");
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
    let (connection_row, connection) = fact_row("connection");
    connection_row.set_visible(false);
    status.append(&connection_row);
    let (reached_row, reached) = fact_row("last reached");
    reached_row.set_visible(false);
    status.append(&reached_row);

    let details = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    details.add_css_class("sol-row");
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

    let page = make_source_page(status);
    pane.append(&page.box_);

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
        connect,
        soft,
        kept_row,
        kept,
        connection_row,
        connection,
        reached_row,
        reached,
        unread,
        page,
    }
}

fn make_source_page(status: gtk::Box) -> Page {
    let page_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page_box.add_css_class("sol-page");
    let home = gtk::Button::with_label("‹ home");
    home.add_css_class("sol-back");
    home.set_has_frame(false);
    home.set_halign(gtk::Align::Start);
    home.set_visible(false);
    page_box.append(&home);

    let page_top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let glyphs = [
        glyph_widget(GLYPH_SCREEN),
        glyph_widget(GLYPH_TERMINAL),
        glyph_widget(GLYPH_BROWSERS),
        glyph_widget(GLYPH_OBSIDIAN),
    ];
    for glyph in &glyphs {
        glyph.set_visible(false);
        page_top.append(glyph);
    }
    let title = gtk::Label::new(None);
    title.add_css_class("sol-name");
    title.add_css_class("sol-display");
    title.add_css_class("sol-ink");
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    title.set_xalign(0.0);
    let close = gtk::Button::with_label("×");
    close.add_css_class("sol-glyph");
    close.set_has_frame(false);
    close.set_tooltip_text(Some("close"));
    close.set_visible(false);
    page_top.append(&title);
    page_top.append(&close);
    page_box.append(&page_top);

    let verdict = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let mark = mark_widget();
    let word = gtk::Label::new(None);
    word.add_css_class("sol-state");
    word.set_halign(gtk::Align::Start);
    let verdict_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    verdict_spacer.set_hexpand(true);
    let switch = gtk::Switch::new();
    switch.add_css_class("sol-switch");
    switch.set_valign(gtk::Align::Center);
    verdict.append(&mark);
    verdict.append(&word);
    verdict.append(&verdict_spacer);
    verdict.append(&switch);
    page_box.append(&verdict);

    let reason = note_label(None);
    let next = note_label(None);
    let breath = note_label(None);
    breath.set_visible(true);
    let sound = note_label(Some(SCREEN_SOUND));
    sound.add_css_class("sol-quiet");
    page_box.append(&reason);
    page_box.append(&next);
    page_box.append(&breath);
    page_box.append(&sound);

    let (so_far_row, so_far) = fact_row("so far");
    let (last_row, last) = fact_row("last taken in");
    page_box.append(&so_far_row);
    page_box.append(&last_row);

    let (
        screen_extra,
        activity,
        titles,
        urls,
        through_pause,
        activity_only,
        exclude_apps,
        exclude_titles,
        save,
        save_note,
    ) = screen_settings();
    let (browsers_extra, browsers_names) = browsers_settings();
    let (obsidian_extra, obsidian_turn_off) = obsidian_settings();
    page_box.append(&screen_extra);
    page_box.append(&browsers_extra);
    page_box.append(&obsidian_extra);

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.add_css_class("sol-page");
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    // The page's own height must not become the window's height.
    scrolled.set_propagate_natural_height(false);
    scrolled.set_propagate_natural_width(false);
    scrolled.set_vexpand(true);
    scrolled.set_visible(false);
    scrolled.set_child(Some(&page_box));

    Page {
        status,
        box_: scrolled,
        home,
        glyphs,
        title,
        mark,
        word,
        switch,
        reason,
        next,
        breath,
        sound,
        so_far_row,
        so_far,
        last_row,
        last,
        screen_extra,
        browsers_extra,
        obsidian_extra,
        obsidian_turn_off,
        browsers_names,
        activity,
        titles,
        urls,
        through_pause,
        activity_only,
        exclude_apps,
        exclude_titles,
        save,
        save_note,
        edited: Rc::new(Cell::new(false)),
        close,
        painting: Rc::new(Cell::new(false)),
    }
}

fn screen_settings() -> (
    gtk::Box,
    gtk::Switch,
    gtk::Switch,
    gtk::Switch,
    gtk::Switch,
    gtk::Switch,
    gtk::Label,
    gtk::Label,
    gtk::Button,
    gtk::Label,
) {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let head = section_label("goes along with it");
    head.add_css_class("sol-rule");
    box_.append(&head);
    let (activity_row, activity) = setting_row(solstone_linux::copy::SETTINGS_ACTIVITY_SOURCE);
    let (titles_row, titles) = setting_row(solstone_linux::copy::SETTINGS_ACTIVITY_TITLES);
    let (urls_row, urls) = setting_row(solstone_linux::copy::SETTINGS_ACTIVITY_URLS);
    let (pause_row, through_pause) =
        setting_row(solstone_linux::copy::SETTINGS_ACTIVITY_THROUGH_PAUSE);
    let (only_row, activity_only) = setting_row(solstone_linux::copy::SETTINGS_ACTIVITY_ONLY);
    box_.append(&activity_row);
    box_.append(&titles_row);
    box_.append(&urls_row);
    box_.append(&pause_row);
    box_.append(&note_label(Some(
        solstone_linux::copy::SETTINGS_ACTIVITY_THROUGH_PAUSE_HELP,
    )));
    box_.append(&only_row);
    box_.append(&note_label(Some(
        solstone_linux::copy::SETTINGS_ACTIVITY_ONLY_HELP,
    )));
    box_.append(&note_label(Some(
        solstone_linux::copy::SETTINGS_RESTART_HELP,
    )));
    let save = gtk::Button::with_label(solstone_linux::copy::SETTINGS_SAVE);
    save.add_css_class("sol-action");
    save.set_halign(gtk::Align::Start);
    box_.append(&save);
    let save_note = note_label(None);
    box_.append(&save_note);
    let left = section_label("left out");
    left.add_css_class("sol-rule");
    box_.append(&left);
    let (apps_row, exclude_apps) = fact_row(solstone_linux::copy::SETTINGS_ACTIVITY_EXCLUDE_APPS);
    let (titles_left, exclude_titles) =
        fact_row(solstone_linux::copy::SETTINGS_ACTIVITY_EXCLUDE_TITLES);
    box_.append(&apps_row);
    box_.append(&titles_left);
    box_.append(&note_label(Some(
        solstone_linux::copy::SETTINGS_ACTIVITY_PRIVATE_WINDOWS_NOTE,
    )));
    (
        box_,
        activity,
        titles,
        urls,
        through_pause,
        activity_only,
        exclude_apps,
        exclude_titles,
        save,
        save_note,
    )
}

fn browsers_settings() -> (gtk::Box, gtk::Label) {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let head = section_label("browsers");
    head.add_css_class("sol-rule");
    box_.append(&head);
    box_.append(&note_label(Some(BROWSERS_GAP)));
    let (row, names) = fact_row("connected");
    names.set_wrap(true);
    box_.append(&row);
    box_.set_visible(false);
    (box_, names)
}

fn obsidian_settings() -> (gtk::Box, gtk::Button) {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let head = section_label(solstone_linux::copy::OBSIDIAN_VAULT_LABEL);
    head.add_css_class("sol-rule");
    box_.append(&head);
    box_.append(&note_label(Some(
        solstone_linux::copy::OBSIDIAN_FOUND_LABEL,
    )));
    box_.append(&note_label(Some(solstone_linux::copy::OBSIDIAN_NONE_FOUND)));
    let choose = gtk::Button::with_label(solstone_linux::copy::OBSIDIAN_CHOOSE_FOLDER);
    choose.add_css_class("sol-action");
    choose.set_halign(gtk::Align::Start);
    box_.append(&choose);
    let turn_off = gtk::Button::with_label(solstone_linux::copy::OBSIDIAN_TURN_OFF);
    turn_off.add_css_class("sol-action");
    turn_off.set_halign(gtk::Align::Start);
    box_.append(&turn_off);
    box_.set_visible(false);
    (box_, turn_off)
}

fn page_breath(id: TileId) -> &'static str {
    match id {
        TileId::Screen => SCREEN_BREATH,
        TileId::Terminal => TERMINAL_BREATH,
        TileId::Browsers => BROWSERS_BREATH,
        TileId::Obsidian => solstone_linux::copy::OBSIDIAN_EXPLAIN,
    }
}

fn page_reason(
    id: TileId,
    word: StateWord,
    view: &View,
) -> (Option<&'static str>, Option<&'static str>) {
    if word != StateWord::NeedsAttention {
        return (None, None);
    }
    match id {
        TileId::Screen => (Some(SCREEN_WAITING), None),
        TileId::Terminal => (view.terminal_reason, None),
        TileId::Browsers => (view.browsers_reason, None),
        TileId::Obsidian => (Some(solstone_linux::copy::OBSIDIAN_ATTENTION_VAULT), None),
    }
}

fn page_facts(id: TileId, file: &IntakeFile) -> (Option<String>, Option<String>) {
    let total = match id {
        TileId::Screen => shell_model::screen_so_far(file),
        TileId::Terminal => shell_model::terminal_so_far(file),
        TileId::Browsers => shell_model::browsers_so_far(file),
        TileId::Obsidian => shell_model::obsidian_so_far(file),
    };
    let started = match file {
        IntakeFile::Rows(rows) => match id {
            TileId::Screen => rows.screen.started.as_deref(),
            TileId::Terminal => rows.terminal.started.as_deref(),
            TileId::Browsers => rows.browsers.started.as_deref(),
            TileId::Obsidian => rows.obsidian.started.as_deref(),
        },
        IntakeFile::Missing | IntakeFile::Refused => None,
    };
    let last_raw = match file {
        IntakeFile::Rows(rows) => counter_last(rows, id),
        IntakeFile::Missing | IntakeFile::Refused => None,
    };
    let last = shell_model::source_last_now(file, last_raw);
    (
        shell_model::page_total_now(&total, started),
        shell_model::last_text(&last).map(str::to_owned),
    )
}

fn count_label(count: usize) -> String {
    if count == 0 {
        String::new()
    } else {
        count.to_string()
    }
}

pub(crate) fn note_label(text: Option<&str>) -> gtk::Label {
    let label = gtk::Label::new(text);
    label.add_css_class("sol-faint");
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_max_width_chars(36);
    label.set_visible(text.is_some());
    label
}

pub(crate) fn fact_row(name: &str) -> (gtk::Box, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("sol-row");
    let label = gtk::Label::new(Some(name));
    label.add_css_class("sol-ink");
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    label.set_wrap(true);
    label.set_xalign(0.0);
    let value = gtk::Label::new(None);
    value.add_css_class("sol-faint");
    value.set_halign(gtk::Align::End);
    value.set_xalign(1.0);
    value.set_wrap(true);
    row.append(&label);
    row.append(&value);
    (row, value)
}

fn setting_row(name: &str) -> (gtk::Box, gtk::Switch) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("sol-row");
    let label = gtk::Label::new(Some(name));
    label.add_css_class("sol-ink");
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    label.set_wrap(true);
    label.set_xalign(0.0);
    let switch = gtk::Switch::new();
    switch.add_css_class("sol-switch");
    switch.set_valign(gtk::Align::Center);
    switch.set_halign(gtk::Align::End);
    label.set_mnemonic_widget(Some(&switch));
    row.append(&label);
    row.append(&switch);
    (row, switch)
}

pub(crate) fn section_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("sol-section");
    label.add_css_class("sol-faint");
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    label
}

fn source_row(name: &str, glyph: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("sol-row");
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
    pause.set_label("pause ▾");
    pause.set_always_show_arrow(false);
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

fn journal_pill() -> (gtk::Box, gtk::Label, gtk::DrawingArea) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    row.set_halign(gtk::Align::Center);
    row.add_css_class("sol-journal");
    let left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    left.add_css_class("sol-chip");
    left.add_css_class("sol-chip-a");
    left.set_size_request(11, 11);
    let right = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    right.add_css_class("sol-chip");
    right.add_css_class("sol-chip-b");
    right.set_size_request(11, 11);
    let label = gtk::Label::new(Some("your · journal"));
    label.add_css_class("sol-display");
    label.add_css_class("sol-ink");
    // A paired journal shows its own mark in place of the two placeholder chips.
    let mark = crate::mark_card::mark_chips_area();
    mark.set_visible(false);
    row.append(&left);
    row.append(&right);
    row.append(&mark);
    row.append(&label);
    (row, label, mark)
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

fn wire_page_switch(
    ui: &Rc<Ui>,
    view: &Rc<RefCell<View>>,
    seen: &Rc<Cell<bool>>,
    daemon: &Rc<Cell<Option<bool>>>,
    selected: &Rc<Cell<Option<TileId>>>,
) {
    let painting = Rc::clone(&ui.page.painting);
    let view = Rc::clone(view);
    let seen = Rc::clone(seen);
    let daemon = Rc::clone(daemon);
    let refresh_ui = Rc::clone(ui);
    let selected = Rc::clone(selected);
    let switch = ui.page.switch.clone();
    switch.connect_state_set(move |switch, on| {
        if painting.get() {
            return glib::Propagation::Proceed;
        }
        let which = match selected.get() {
            Some(TileId::Screen) => Some(OwnerSwitch::Screen),
            Some(TileId::Terminal) => Some(OwnerSwitch::Terminal),
            _ => None,
        };
        let Some(which) = which else {
            painting.set(true);
            switch.set_active(!on);
            painting.set(false);
            return glib::Propagation::Stop;
        };
        if !switch.is_sensitive() {
            return glib::Propagation::Proceed;
        }
        switch.set_sensitive(false);
        let switch = switch.clone();
        let painting = Rc::clone(&painting);
        let refresh_ui = Rc::clone(&refresh_ui);
        let view = Rc::clone(&view);
        let seen = Rc::clone(&seen);
        let daemon = Rc::clone(&daemon);
        let selected = Rc::clone(&selected);
        glib::MainContext::default().spawn_local(async move {
            let done = save_owner_switch(which, on).await;
            switch.set_sensitive(true);
            refresh(&refresh_ui, &view, &seen, &daemon, &selected);
            if !done {
                let saved = owner_switch_value(&view.borrow(), which);
                painting.set(true);
                switch.set_active(saved);
                painting.set(false);
            }
        });
        glib::Propagation::Proceed
    });
}

/// The screen page's activity switches are a draft until "save settings", which
/// sends the changed ones to the running app the way the settings form did.
fn wire_activity_save(
    ui: &Rc<Ui>,
    view: &Rc<RefCell<View>>,
    seen: &Rc<Cell<bool>>,
    daemon: &Rc<Cell<Option<bool>>>,
    selected: &Rc<Cell<Option<TileId>>>,
) {
    let page = &ui.page;
    for switch in [
        &page.activity,
        &page.titles,
        &page.urls,
        &page.through_pause,
        &page.activity_only,
    ] {
        let painting = Rc::clone(&page.painting);
        let edited = Rc::clone(&page.edited);
        let note = page.save_note.clone();
        switch.connect_state_set(move |_, _| {
            if !painting.get() {
                edited.set(true);
                note.set_visible(false);
            }
            glib::Propagation::Proceed
        });
    }
    let ui = Rc::clone(ui);
    let view = Rc::clone(view);
    let seen = Rc::clone(seen);
    let daemon = Rc::clone(daemon);
    let selected = Rc::clone(selected);
    page.save.clone().connect_clicked(move |button| {
        let page = &ui.page;
        let saved = view.borrow();
        let mut policy = serde_json::Map::new();
        for (key, switch, was) in [
            ("activity_source", &page.activity, saved.activity_source),
            ("activity_titles", &page.titles, saved.activity_titles),
            ("activity_urls", &page.urls, saved.activity_urls),
            (
                "activity_through_pause",
                &page.through_pause,
                saved.activity_through_pause,
            ),
        ] {
            if switch.is_active() != was {
                policy.insert(key.into(), switch.is_active().into());
            }
        }
        let mut writes = Vec::new();
        if !policy.is_empty() {
            writes.push((
                "activity_policy",
                serde_json::Value::Object(policy).to_string(),
            ));
        }
        if page.activity_only.is_active() != saved.activity_only {
            writes.push(("activity_only", page.activity_only.is_active().to_string()));
        }
        drop(saved);
        if writes.is_empty() {
            page.edited.set(false);
            return;
        }
        button.set_sensitive(false);
        button.set_label(solstone_linux::copy::SETTINGS_SAVING);
        let button = button.clone();
        let ui = Rc::clone(&ui);
        let view = Rc::clone(&view);
        let seen = Rc::clone(&seen);
        let daemon = Rc::clone(&daemon);
        let selected = Rc::clone(&selected);
        glib::MainContext::default().spawn_local(async move {
            let done = write_settings(&writes).await;
            button.set_label(solstone_linux::copy::SETTINGS_SAVE);
            button.set_sensitive(true);
            let note = &ui.page.save_note;
            if done {
                ui.page.edited.set(false);
                note.set_text(solstone_linux::copy::SETTINGS_SAVED);
            } else {
                note.set_text(solstone_linux::copy::SETTINGS_SAVE_UNAVAILABLE);
            }
            note.set_visible(true);
            refresh(&ui, &view, &seen, &daemon, &selected);
        });
    });
}

/// The this-computer switches save as they flip, through the running app the way
/// the settings form did. A write the app does not take repaints the saved value.
fn wire_computer(
    ui: &Rc<Ui>,
    view: &Rc<RefCell<View>>,
    seen: &Rc<Cell<bool>>,
    daemon: &Rc<Cell<Option<bool>>>,
    selected: &Rc<Cell<Option<TileId>>>,
) {
    for (switch, field) in [
        (&ui.cursor, "draw_cursor"),
        (&ui.start_paused, "start_paused"),
    ] {
        let ui = Rc::clone(ui);
        let view = Rc::clone(view);
        let seen = Rc::clone(seen);
        let daemon = Rc::clone(daemon);
        let selected = Rc::clone(selected);
        switch.connect_state_set(move |switch, on| {
            // A repaint is not an edit, and a switch already saving waits.
            if ui.computer_painting.get() || !switch.is_sensitive() {
                return glib::Propagation::Proceed;
            }
            ui.computer_note.set_visible(false);
            switch.set_sensitive(false);
            let switch = switch.clone();
            let ui = Rc::clone(&ui);
            let view = Rc::clone(&view);
            let seen = Rc::clone(&seen);
            let daemon = Rc::clone(&daemon);
            let selected = Rc::clone(&selected);
            glib::MainContext::default().spawn_local(async move {
                let done = write_settings(&[(field, on.to_string())]).await;
                switch.set_sensitive(true);
                refresh(&ui, &view, &seen, &daemon, &selected);
                if !done || saved_value(&view.borrow(), field) != on {
                    ui.computer_note
                        .set_text(solstone_linux::copy::SETTINGS_SAVE_UNAVAILABLE);
                    ui.computer_note.set_visible(true);
                }
            });
            glib::Propagation::Proceed
        });
    }
}

fn saved_value(view: &View, field: &str) -> bool {
    if field == "draw_cursor" {
        view.draw_cursor
    } else {
        view.start_paused
    }
}

/// True only when the app took every write without refusing it.
async fn write_settings(writes: &[(&str, String)]) -> bool {
    let Ok(client) = solstone_linux::dbus_client::ControlClient::session().await else {
        return false;
    };
    for (field, value) in writes {
        let Ok(reply) = client.write_setting(field, value).await else {
            return false;
        };
        if !setting_taken(&reply) {
            return false;
        }
    }
    true
}

/// The app saved the write and did not refuse it.
fn setting_taken(reply: &str) -> bool {
    matches!(
        solstone_linux::window_control::decode_control_str(reply),
        Ok(model) if model.saved_available
            && !matches!(
                model.fault.as_deref(),
                Some("invalid-setting" | "invalid-config")
            )
    )
}

fn computer_page() -> (gtk::Box, gtk::Switch, gtk::Switch, gtk::Label, gtk::Label) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.add_css_class("sol-pane");
    page.add_css_class("sol-flush");
    page.set_visible(false);
    page.set_hexpand(true);
    page.set_vexpand(true);
    page.set_valign(gtk::Align::Fill);
    let title = gtk::Label::new(Some("this computer"));
    title.add_css_class("sol-name");
    title.add_css_class("sol-display");
    title.add_css_class("sol-ink");
    title.set_halign(gtk::Align::Start);
    page.append(&title);
    let (cursor_row, cursor) = setting_row(solstone_linux::copy::SETTINGS_CURSOR);
    let (paused_row, start_paused) = setting_row(solstone_linux::copy::SETTINGS_START_PAUSED);
    let (segment_row, segment) = fact_row(solstone_linux::copy::SETTINGS_SEGMENT);
    page.append(&cursor_row);
    page.append(&paused_row);
    let note = note_label(None);
    page.append(&note);
    page.append(&segment_row);
    let (file_row, file_value) = fact_row(solstone_linux::copy::SETTINGS_FILE);
    let loaded = config::load_config(config::ConfigPaths::default());
    file_value.set_text(&loaded.config.config_path().display().to_string());
    file_value.set_visible(true);
    page.append(&file_row);
    (page, cursor, start_paused, segment, note)
}

/// The app's version, the paired journal's version when it is known, and how to
/// update. There is no updater here: the installer's `--upgrade` updates the app.
fn updates_page() -> (gtk::Box, gtk::Box, gtk::Label) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page.add_css_class("sol-pane");
    page.add_css_class("sol-flush");
    page.set_visible(false);
    page.set_hexpand(true);
    page.set_vexpand(true);
    page.set_valign(gtk::Align::Fill);
    let title = gtk::Label::new(Some("updates"));
    title.add_css_class("sol-name");
    title.add_css_class("sol-display");
    title.add_css_class("sol-ink");
    title.set_halign(gtk::Align::Start);
    page.append(&title);
    let (app_row, app_version) = fact_row(solstone_linux::copy::STATUS_VERSION);
    app_version.set_text(env!("CARGO_PKG_VERSION"));
    page.append(&app_row);
    let (journal_row, journal_version) = fact_row(solstone_linux::copy::UPDATES_JOURNAL_VERSION);
    journal_row.set_visible(false);
    page.append(&journal_row);
    page.append(&note_label(Some(solstone_linux::copy::UPDATES_HOW)));
    (page, journal_row, journal_version)
}

/// The journal's version is the one the about lines read: only a version saved
/// for the journal this computer is paired with now.
fn paint_updates(ui: &Ui) {
    let config = config::load_config(config::ConfigPaths::default()).config;
    let about = solstone_linux::about::AboutBlock::snapshot(
        &config,
        &solstone_linux::about::host_facts(),
        solstone_linux::about::now(),
    );
    match about.journal_version {
        Some(version) => {
            ui.updates_journal.set_text(&version);
            ui.updates_journal_row.set_visible(true);
        }
        None => ui.updates_journal_row.set_visible(false),
    }
}

fn place_shelf_button(body: &gtk::Box, button: &gtk::Button) {
    let Some(parent) = body.parent() else {
        return;
    };
    let Ok(overlay) = parent.downcast::<gtk::Overlay>() else {
        return;
    };
    button.set_halign(gtk::Align::Start);
    button.set_valign(gtk::Align::Start);
    button.set_margin_start(sun_bg::INSET + 4);
    button.set_margin_top(10);
    overlay.add_overlay(button);
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
    switch.connect_state_set(move |switch, on| {
        // A repaint is not an edit, and a switch already saving waits.
        if ui.tile_painting.get() || !switch.is_sensitive() {
            return glib::Propagation::Proceed;
        }
        switch.set_sensitive(false);
        let switch = switch.clone();
        let ui = Rc::clone(&ui);
        let view = Rc::clone(&view);
        let seen = Rc::clone(&seen);
        let daemon = Rc::clone(&daemon);
        let selected = Rc::clone(&selected);
        glib::MainContext::default().spawn_local(async move {
            let done = save_owner_switch(which, on).await;
            switch.set_sensitive(true);
            let terminal_on = done && which == OwnerSwitch::Terminal && on;
            if terminal_on {
                daemon.set(None);
            }
            refresh(&ui, &view, &seen, &daemon, &selected);
            if !done {
                let saved = owner_switch_value(&view.borrow(), which);
                ui.tile_painting.set(true);
                switch.set_active(saved);
                ui.tile_painting.set(false);
            }
            if terminal_on {
                probe_daemon(
                    Rc::clone(&daemon),
                    Rc::clone(&view),
                    Rc::clone(&seen),
                    Rc::clone(&ui),
                    Rc::clone(&selected),
                );
            }
        });
        glib::Propagation::Proceed
    });
}

/// Screen and terminal go to the running app, which saves the switch and applies it
/// at once. With no app running there is nothing to apply it to, so the switch is
/// saved for its next start, the way the command line does it.
async fn save_owner_switch(which: OwnerSwitch, on: bool) -> bool {
    let field = match which {
        OwnerSwitch::Screen => "desktop_capture",
        OwnerSwitch::Terminal => "terminal",
    };
    let save_for_start =
        || config::set_owner_switch(&config::ConfigPaths::default(), which, on).is_ok();
    let Ok(client) = solstone_linux::dbus_client::ControlClient::session().await else {
        return save_for_start();
    };
    match client.write_setting(field, &on.to_string()).await {
        Ok(reply) => setting_taken(&reply),
        Err(solstone_linux::dbus_client::ControlClientError::NoOwner) => save_for_start(),
        Err(_) => false,
    }
}

fn owner_switch_value(view: &View, which: OwnerSwitch) -> bool {
    match which {
        OwnerSwitch::Screen => view.screen_on,
        OwnerSwitch::Terminal => view.terminal_on,
    }
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
    QUIT_ROW.with(|slot| {
        if let Some(row) = slot.borrow().as_ref() {
            row.set_visible(quit_on_help());
        }
    });
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
            if switch.is_active() != on && !switch.has_focus() && switch.is_sensitive() {
                ui.tile_painting.set(true);
                switch.set_active(on);
                ui.tile_painting.set(false);
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
    // A switch waiting on its save keeps the owner's flip until the app answers.
    ui.computer_painting.set(true);
    if ui.cursor.is_sensitive() {
        ui.cursor.set_active(view.draw_cursor);
    }
    if ui.start_paused.is_sensitive() {
        ui.start_paused.set_active(view.start_paused);
    }
    ui.computer_painting.set(false);
    ui.segment.set_text(&format!(
        "{} {}",
        view.segment_interval,
        solstone_linux::copy::SETTINGS_SEGMENT_HELP
    ));
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
    let measured = ui.window.width();
    let width = if measured > 1 {
        measured
    } else {
        ui.window.default_width()
    };
    let class = shell_model::width_class(width);
    let phone = class == WidthClass::Phone;
    let tuck = shell_model::shelf_button(width);
    if tuck {
        ui.shelf_button.set_visible(true);
        ui.shelf.set_visible(shelf_open.get());
        // The open shelf floats over the deck. Reserving its width pushes the
        // second column under the pane.
        ui.body.set_margin_start(sun_bg::INSET);
        if shelf_open.get() {
            ui.shelf.add_css_class("sol-shelf-over");
        } else {
            ui.shelf.remove_css_class("sol-shelf-over");
        }
    } else {
        ui.shelf.remove_css_class("sol-shelf-over");
        ui.shelf_button.set_visible(false);
        ui.shelf.set_visible(true);
        ui.body
            .set_margin_start(sun_bg::INSET + sun_bg::SHELF_WIDTH + sun_bg::INSET);
    }
    let cols = i32::from(shell_model::deck_columns(width));
    place_tiles(&ui.grid, &ui.tiles, &ui.add_more, cols);
    let home = row.get() == ShelfRow::Home;
    let computer = row.get() == ShelfRow::Computer;
    ui.grid.set_visible(home);
    if let Some(column) = deck_column(&ui.grid) {
        column.set_visible(home);
    }
    let journal = row.get() == ShelfRow::Journal;
    ui.other_box.set_visible(journal);
    ui.journal_slot.set_visible(journal);
    ui.help_slot.set_visible(row.get() == ShelfRow::Help);
    ui.computer.set_visible(computer);
    let updates = row.get() == ShelfRow::Updates;
    if updates {
        paint_updates(ui);
    }
    ui.updates.set_visible(updates);
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
        ui.body.set_margin_end(PANE_WIDTH + PANE_GUTTER);
    } else if pane_open && phone {
        ui.pane.set_halign(gtk::Align::Fill);
        ui.pane.set_size_request(-1, -1);
        ui.pane.set_margin_start(ui.body.margin_start());
        ui.pane.set_margin_top(sun_bg::HEADER_HEIGHT);
        ui.pane.set_margin_end(ui.body.margin_start());
        ui.pane.set_margin_bottom(0);
        ui.body.set_margin_end(sun_bg::INSET);
    } else {
        ui.body.set_margin_end(sun_bg::INSET);
    }
    let phone_page = phone && page_open.get();
    // The desktop close sits on this corner. The page's way back is a link.
    ui.page.close.set_visible(false);
    if page_open.get() {
        ui.page
            .home
            .set_label(if phone_page { "‹ home" } else { "‹ status" });
        ui.page.home.set_visible(true);
    } else {
        ui.page.home.set_visible(false);
    }
    if phone && pane_open {
        ui.pane.add_css_class("sol-flush");
    } else {
        ui.pane.remove_css_class("sol-flush");
    }
}

fn deck_column(grid: &gtk::Grid) -> Option<gtk::Widget> {
    let mut current = grid.parent()?;
    for _ in 0..6 {
        if current.has_css_class("sol-deck-column") {
            return Some(current);
        }
        current = current.parent()?;
    }
    None
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

/// Frame captures set a width, an open shelf, or a source page. Unset does nothing.
fn apply_shot_hooks(
    ui: &Ui,
    view: &Rc<RefCell<View>>,
    selected: &Cell<Option<TileId>>,
    row: &Cell<ShelfRow>,
    shelf_open: &Cell<bool>,
    page_open: &Cell<bool>,
) {
    if let Ok(raw) = std::env::var("SOLSTONE_WINDOW_WIDTH")
        && let Ok(width) = raw.parse::<i32>()
        && (400..4000).contains(&width)
    {
        ui.window.set_default_width(width);
    }
    if let Ok(raw) = std::env::var("SOLSTONE_WINDOW_HEIGHT")
        && let Ok(height) = raw.parse::<i32>()
        && (480..2000).contains(&height)
    {
        ui.window.set_default_height(height);
    }
    if std::env::var("SOLSTONE_SHELF_OPEN").ok().as_deref() == Some("1") {
        shelf_open.set(true);
    }
    let Ok(which) = std::env::var("SOLSTONE_OPEN_PAGE") else {
        return;
    };
    if which == "computer" {
        row.set(ShelfRow::Computer);
        return;
    }
    let id = match which.as_str() {
        "screen" => Some(TileId::Screen),
        "terminal" => Some(TileId::Terminal),
        "browsers" => Some(TileId::Browsers),
        "obsidian" => Some(TileId::Obsidian),
        _ => None,
    };
    if let Some(id) = id {
        selected.set(Some(id));
        page_open.set(true);
        ui.page.show_tile(id, &view.borrow());
    }
}

fn read_view(seen_report: &Cell<bool>, daemon_up: Option<bool>) -> View {
    let loaded = config::load_config(config::ConfigPaths::default());
    let config = loaded.config;
    let intake = source_intake::read_rows(&config.state_dir().join(source_intake::FILENAME));
    let snap = current_snap();
    let paused = pause_left(&config, &snap);
    let app_paused = paused.is_some();
    let activity_running = if snap.ready {
        shell_model::activity_running(
            snap.activity_source,
            config.activity_only,
            app_paused,
            snap.activity_through_pause,
        )
    } else {
        shell_model::activity_running(
            config.activity_source,
            config.activity_only,
            app_paused,
            config.activity_through_pause,
        )
    };
    let screen = screen_now(&config, app_paused, activity_running, &snap);
    let (terminal, terminal_reason) = terminal_word(&config, daemon_up, app_paused);
    let (browsers, browsers_reason, browsers_sub) = browsers_word(&config, seen_report);
    let obsidian = obsidian_word(&snap);
    let (mut paired, mut offline, mut offline_waiting) = journal_face(&snap);
    let mut connection = paired.then(|| connection_words(&snap.health).to_owned());
    let mut last_reached = if paired {
        snap.last_sync.and_then(shell_model::relative_unix_now)
    } else {
        None
    };
    let mut fixture_mark = false;
    apply_journal_fixture(
        &mut paired,
        &mut offline,
        &mut offline_waiting,
        &mut connection,
        &mut last_reached,
        &mut fixture_mark,
    );
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
        fixture_mark,
        mark: snap.mark.clone(),
        offline,
        offline_waiting,
        connection,
        last_reached,
        kept: snap.kept,
        activity_source: config.activity_source,
        activity_titles: config.activity_titles,
        activity_urls: config.activity_urls,
        activity_through_pause: config.activity_through_pause,
        activity_only: config.activity_only,
        exclude_apps: config.activity_exclude_apps.len(),
        exclude_titles: config.activity_exclude_titles.len(),
        draw_cursor: config.draw_cursor,
        start_paused: config.start_paused,
        segment_interval: config.segment_interval,
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
            sub: if activity_running {
                ScreenSub::WithAudioAndActivity
            } else {
                ScreenSub::WithAudio
            },
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

/// A frame can show a paired journal this desktop has not reached. A normal
/// launch leaves the live reading alone.
fn apply_journal_fixture(
    paired: &mut bool,
    offline: &mut bool,
    waiting: &mut Option<i64>,
    connection: &mut Option<String>,
    last_reached: &mut Option<String>,
    named: &mut bool,
) {
    *named = false;
    let framed = std::env::var("SOLSTONE_FRAME_PNG")
        .ok()
        .is_some_and(|value| !value.is_empty());
    if !framed {
        return;
    }
    let Ok(raw) = std::env::var("SOLSTONE_JOURNAL") else {
        return;
    };
    match raw.as_str() {
        "connected" => {
            *paired = true;
            *offline = false;
            *waiting = None;
            *connection = Some(solstone_linux::copy::JOURNAL_CONNECTED.to_owned());
            *last_reached = fixture_reached(0);
            *named = true;
        }
        "offline" => {
            *paired = true;
            *offline = true;
            *waiting = None;
            *connection = Some(solstone_linux::copy::JOURNAL_OFFLINE.to_owned());
            *last_reached = fixture_reached(40 * 60);
            *named = true;
        }
        _ => {
            let Some(count) = raw.strip_prefix("offline:") else {
                return;
            };
            let Ok(count) = count.parse::<i64>() else {
                return;
            };
            if count <= 0 {
                return;
            }
            *paired = true;
            *offline = true;
            *waiting = Some(count);
            *connection = Some(solstone_linux::copy::JOURNAL_OFFLINE.to_owned());
            *last_reached = fixture_reached(40 * 60);
            *named = true;
        }
    }
}

/// The fixture's reached line, on the same scale as a real sync. `0` is just now.
fn fixture_reached(seconds_ago: u32) -> Option<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    shell_model::relative_unix_now(now - f64::from(seconds_ago))
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

/// A running app's own word decides whether it is paused. `start paused` only says
/// how the app starts, so with no app answering, this is the pause it would start in.
fn pause_left(config: &solstone_linux::config::Config, snap: &ServiceSnap) -> Option<PauseLeft> {
    let wall = unix_secs();
    let until_wall = if snap.ready {
        snap.pause_bounded?;
        match pause_hold::load(&config.state_dir()) {
            Some(HeldPause::Until(deadline)) if deadline > wall => Some(deadline),
            _ => None,
        }
    } else {
        let starting = pause_hold::StartingPause::read(config, wall, wall);
        if !starting.paused {
            return None;
        }
        starting.until_wall
    };
    match until_wall {
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
.sol-pane-title.sol-display { font-size: 19px; }\
.sol-lead.sol-display { font-size: 22px; }\
.sol-journal .sol-display { font-size: 13.5px; }\
.sol-state { font-size: 13.5px; font-weight: 600; }\
.sol-section { font-size: 12px; font-weight: 600; }\
.sol-rule {\
  border-bottom: 1px solid rgba(58, 46, 36, 0.14);\
  padding-bottom: 4px;\
  margin-top: 16px;\
}\
window.sol-dark .sol-rule {\
  border-bottom-color: rgba(255, 244, 230, 0.16);\
}\
.sol-row {\
  border-bottom: 1px solid rgba(58, 46, 36, 0.14);\
  padding-top: 7px;\
  padding-bottom: 7px;\
}\
window.sol-dark .sol-row {\
  border-bottom-color: rgba(255, 244, 230, 0.16);\
}\
button.sol-action {\
  background-color: #FEFCF8;\
  background-image: none;\
  border: 1px solid rgba(58, 46, 36, 0.16);\
  border-radius: 999px;\
  box-shadow: none;\
  padding: 6px 14px;\
  font-size: 13.5px;\
  font-weight: 600;\
  min-height: 0;\
  min-width: 0;\
  color: #3A2E24;\
}\
button.sol-action:hover, button.sol-action:active, button.sol-action:checked {\
  background-color: #FEFCF8;\
  background-image: none;\
  box-shadow: none;\
  color: #3A2E24;\
}\
window.sol-dark button.sol-action,\
window.sol-dark button.sol-action:hover,\
window.sol-dark button.sol-action:active {\
  background-color: #2A221C;\
  color: #F6EDE3;\
  border-color: rgba(255, 244, 230, 0.22);\
}\
button.sol-action:disabled { opacity: 0.5; }\
scrolledwindow.sol-log {\
  background-color: #FCF3E4;\
  border: 1px solid rgba(58, 46, 36, 0.14);\
  border-radius: 12px;\
}\
textview.sol-log, textview.sol-log text {\
  background-color: #FCF3E4;\
  color: #3A2E24;\
  font-family: monospace;\
  font-size: 12px;\
}\
window.sol-dark scrolledwindow.sol-log {\
  background-color: #221C19;\
  border-color: rgba(255, 244, 230, 0.16);\
}\
window.sol-dark textview.sol-log, window.sol-dark textview.sol-log text {\
  background-color: #221C19;\
  color: #F6EDE3;\
}\
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
.sol-tile.sol-selected, window.sol-dark .sol-tile.sol-selected {\
  border-color: #E8913A;\
  border-width: 2px;\
}\
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
/* The 360 width is the widget's size request. A CSS min-width is the content box, so it would add this padding and cover the deck. */\
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
button.sol-pill {\
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
menubutton.sol-pill {\
  background: none;\
  background-image: none;\
  border: none;\
  box-shadow: none;\
  padding: 0;\
  min-height: 0;\
  min-width: 0;\
}\
menubutton.sol-pill > button {\
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
menubutton.sol-pill arrow {\
  opacity: 0;\
  min-width: 0;\
  min-height: 0;\
  margin: 0;\
  padding: 0;\
}\
button.sol-pill:hover, button.sol-pill:active, button.sol-pill:checked,\
menubutton.sol-pill > button:hover, menubutton.sol-pill > button:active,\
menubutton.sol-pill > button:checked {\
  background-color: #FEFCF8;\
  background-image: none;\
  box-shadow: none;\
  color: #3A2E24;\
}\
window.sol-dark button.sol-pill,\
window.sol-dark button.sol-pill:hover,\
window.sol-dark menubutton.sol-pill > button,\
window.sol-dark menubutton.sol-pill > button:hover {\
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
.sol-tile:focus, button.sol-pill:focus, .sol-shelf-row:focus, switch.sol-switch:focus,\
menubutton.sol-pill:focus, menubutton.sol-pill > button:focus, button.sol-action:focus,\
button.sol-glyph:focus, button.sol-back:focus {\
  outline: none;\
  box-shadow: none;\
}\
.sol-tile:focus-visible, button.sol-pill:focus-visible, .sol-shelf-row:focus-visible,\
switch.sol-switch:focus-visible, menubutton.sol-pill > button:focus-visible,\
button.sol-action:focus-visible, button.sol-glyph:focus-visible, button.sol-back:focus-visible {\
  outline: 2px solid #E8913A;\
  outline-offset: 2px;\
}\
.sol-quiet { font-size: 12.5px; }\
.sol-shelf.sol-shelf-over {\
  background-color: #FEFCF8;\
  background-image: none;\
  padding: 44px 8px 10px;\
  box-shadow: 0 14px 40px rgba(60, 40, 10, 0.30);\
}\
window.sol-dark .sol-shelf.sol-shelf-over {\
  background-color: #2A221C;\
  box-shadow: 0 14px 40px rgba(0, 0, 0, 0.45);\
}\
.sol-shelf-row { padding: 7px 9px; }\
.sol-journal {\
  background-color: #FEFCF8;\
  border: 1px solid rgba(58, 46, 36, 0.16);\
  border-radius: 999px;\
  padding: 7px 16px;\
}\
window.sol-dark .sol-journal {\
  background-color: #2A221C;\
  border-color: rgba(255, 244, 230, 0.22);\
}\
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
.sol-journal.sol-named .sol-chip {\
  border-style: solid;\
  border-width: 1.5px;\
  min-width: 18px;\
  min-height: 18px;\
  border-radius: 5px;\
}\
.sol-journal.sol-named .sol-chip-a {\
  background-color: #ECE9FB;\
  border-color: #6D5BD0;\
}\
.sol-journal.sol-named .sol-chip-b {\
  background-color: #DFF5F0;\
  border-color: #1F9E89;\
  transform: rotate(45deg) scale(0.82);\
}\
.sol-pane.sol-flush { border-width: 0; }\
scrolledwindow.sol-page, scrolledwindow.sol-deck-scroll {\
  background: none;\
  background-color: transparent;\
  border: none;\
}\
button.sol-glyph, button.sol-glyph:hover, button.sol-glyph:active, button.sol-glyph:checked {\
  background: none;\
  background-image: none;\
  border: none;\
  box-shadow: none;\
  padding: 0;\
  min-height: 0;\
  min-width: 0;\
  color: #3A2E24;\
}\
window.sol-dark button.sol-glyph,\
window.sol-dark button.sol-glyph:hover {\
  color: #F6EDE3;\
}\
button.sol-back, button.sol-back:hover, button.sol-back:active, button.sol-back:checked {\
  background: none;\
  background-image: none;\
  border: none;\
  box-shadow: none;\
  padding: 0;\
  min-height: 0;\
  min-width: 0;\
  color: #E8913A;\
  font-weight: 600;\
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
    /// The status pill when the journal is caught up. Source tiles stay orange.
    Good,
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
        "sol-mark-good",
        "sol-mark-ring",
        "sol-mark-attn",
        "sol-mark-paused",
    ] {
        area.remove_css_class(class);
    }
    let class = match kind {
        MarkKind::Dot => "sol-mark-on",
        MarkKind::Good => "sol-mark-good",
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
    } else if area.has_css_class("sol-mark-good") {
        MarkKind::Good
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
        MarkKind::Good => {
            // tokens.md `--success`
            cr.set_source_rgb(63.0 / 255.0, 157.0 / 255.0, 106.0 / 255.0);
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

fn connection_words(health: &str) -> &'static str {
    match health {
        "connected" | "syncing" => solstone_linux::copy::JOURNAL_CONNECTED,
        "connecting" | "listener-ready" => solstone_linux::copy::LOCAL_JOURNAL_CONNECTING,
        _ => solstone_linux::copy::JOURNAL_OFFLINE,
    }
}

fn place_fact_value(row: &gtk::Box, value: &gtk::Label, text: &str) {
    value.set_text(text);
    value.set_wrap(true);
    // A long value drops under its label. A short one stays on the row.
    // Either way the pane stays 360 wide.
    if text.chars().count() > 22 {
        row.set_orientation(gtk::Orientation::Vertical);
        value.set_halign(gtk::Align::Start);
        value.set_xalign(0.0);
        value.set_max_width_chars(28);
    } else {
        row.set_orientation(gtk::Orientation::Horizontal);
        value.set_halign(gtk::Align::End);
        value.set_xalign(1.0);
        value.set_max_width_chars(22);
    }
}

fn paint_journal_facts(ui: &Ui, view: &View) {
    if !view.paired {
        ui.kept_row.set_visible(true);
        ui.connection_row.set_visible(false);
        ui.reached_row.set_visible(false);
        return;
    }
    ui.kept_row.set_visible(false);
    if let Some(text) = &view.connection {
        place_fact_value(&ui.connection_row, &ui.connection, text);
        ui.connection.set_visible(true);
        ui.connection_row.set_visible(true);
    } else {
        ui.connection_row.set_visible(false);
    }
    if let Some(text) = &view.last_reached {
        place_fact_value(&ui.reached_row, &ui.reached, text);
        ui.reached.set_visible(true);
        ui.reached_row.set_visible(true);
    } else {
        ui.reached_row.set_visible(false);
    }
}

fn paint_journal_name(ui: &Ui, view: &View) {
    let mark = view
        .mark
        .as_ref()
        .filter(|_| view.paired && !view.fixture_mark);
    ui.journal_mark.set_visible(mark.is_some());
    let mut chip = ui.journal.first_child();
    while let Some(widget) = chip {
        if widget.has_css_class("sol-chip") {
            widget.set_visible(mark.is_none());
        }
        chip = widget.next_sibling();
    }
    if let Some(mark) = mark {
        if ui.journal_mark_shown.borrow().as_ref() != Some(mark) {
            crate::mark_card::show_mark_chips(&ui.journal_mark, mark);
            ui.journal_mark.queue_draw();
            *ui.journal_mark_shown.borrow_mut() = Some(mark.clone());
        }
        ui.journal.remove_css_class("sol-named");
        ui.journal.remove_css_class("sol-paired");
        ui.journal_label.set_text(&format!(
            "{} · {}",
            mark.words[0].to_lowercase(),
            mark.words[1].to_lowercase()
        ));
        return;
    }
    if view.fixture_mark {
        ui.journal.add_css_class("sol-named");
        ui.journal.remove_css_class("sol-paired");
        ui.journal_label.set_text("bright · harbor");
        return;
    }
    ui.journal.remove_css_class("sol-named");
    ui.journal_label.set_text("your · journal");
    if view.paired {
        ui.journal.add_css_class("sol-paired");
    } else {
        ui.journal.remove_css_class("sol-paired");
    }
}

fn paint_pairing(ui: &Ui, view: &View) {
    paint_journal_facts(ui, view);
    paint_journal_name(ui, view);
    if !view.paired {
        ui.pill_label.set_text("not paired");
        ui.lead.set_text("not paired");
        show_connect(ui, Some("connect a journal"));
        apply_mark_kind(&ui.pill_mark, MarkKind::Ring);
        return;
    }
    if view.offline {
        if let Some(count) = view.offline_waiting {
            let line = format!("offline · {count} waiting");
            ui.pill_label.set_text(&line);
            ui.lead.set_text(&line);
            show_connect(ui, Some("on this device"));
            apply_mark_kind(&ui.pill_mark, MarkKind::Ring);
        } else {
            ui.pill_label.set_text("");
            ui.lead.set_text("");
            show_connect(ui, None);
            apply_mark_kind(&ui.pill_mark, MarkKind::Ring);
        }
        return;
    }
    ui.pill_label.set_text("connected");
    ui.lead.set_text("all caught up");
    show_connect(ui, Some("everything is in your journal"));
    apply_mark_kind(&ui.pill_mark, MarkKind::Good);
}

fn show_connect(ui: &Ui, text: Option<&str>) {
    match text {
        Some("connect a journal") => {
            ui.connect.set_visible(true);
            ui.soft.set_visible(false);
        }
        Some(text) => {
            ui.connect.set_visible(false);
            ui.soft.set_text(text);
            ui.soft.set_visible(true);
        }
        None => {
            ui.connect.set_visible(false);
            ui.soft.set_text("");
            ui.soft.set_visible(false);
        }
    }
}

fn tile_subs(view: &View) -> [Option<String>; 4] {
    let off_sub = |word: StateWord| (word == StateWord::Off).then(|| OFF_SUB.to_owned());
    let screen = shell_model::screen_sub_label(view.screen_sub)
        .map(str::to_owned)
        .or_else(|| off_sub(view.screen));
    let terminal = view
        .terminal_reason
        .map(str::to_owned)
        .or_else(|| off_sub(view.terminal));
    let browsers = view
        .browsers_reason
        .map(str::to_owned)
        .or_else(|| view.browsers_sub.clone())
        .or_else(|| off_sub(view.browsers));
    let obsidian = off_sub(view.obsidian);
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
        activity_through_pause: doc.settings.activity_through_pause,
        durable: doc.durable,
        health: doc.health,
        last_sync: doc.last_delivery.as_ref().and_then(|item| item.at),
        queue: doc.queue_count,
        obsidian: decoded.as_ref().map(|document| document.status.state),
        obsidian_vault: decoded
            .as_ref()
            .is_some_and(|document| document.status.vault.is_some()),
        kept: "nothing yet".to_owned(),
        pause_bounded: doc.pause_bounded,
        mark: doc.confirmed_mark,
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
