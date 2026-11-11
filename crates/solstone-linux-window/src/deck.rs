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
use solstone_linux::pause_hold::{self, HeldPause};
use solstone_linux::shell_model::{
    self, BrowserHost, PauseFor, PauseLeft, ScreenSub, StateWord, TileId, WidthClass,
};
use solstone_linux::source_intake::{self, IntakeFile, IntakeRows};
use solstone_linux::terminal::state::{EngineState, TerminalState};

use crate::sun_bg::{self, Chrome};

const PANE_WIDTH: i32 = 300;

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
}

struct TileWidgets {
    root: gtk::Box,
    word: gtk::Label,
    sub: gtk::Label,
    switch: Option<gtk::Switch>,
    id: TileId,
}

struct FactWidgets {
    so_far: gtk::Label,
    last_row: gtk::Box,
    last: gtk::Label,
}

pub fn mount(window: &gtk::ApplicationWindow, chrome: &Chrome) {
    let seen_report = Rc::new(Cell::new(false));
    let daemon_up = Rc::new(Cell::new(None));
    let view = Rc::new(RefCell::new(read_view(&seen_report, daemon_up.get())));
    let selected: Rc<Cell<Option<TileId>>> = Rc::new(Cell::new(None));
    let row = Rc::new(Cell::new(ShelfRow::Home));
    let shelf_open = Rc::new(Cell::new(false));
    let page_open = Rc::new(Cell::new(false));

    let greet = gtk::Label::new(Some(shell_model::greeting_now()));
    greet.add_css_class("sol-greet");
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

    let pill = gtk::Button::with_label(shell_model::state_label(StateWord::Checking));
    pill.add_css_class("sol-faint");
    let greet_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    greet_row.append(&greet_text);
    let greet_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    greet_spacer.set_hexpand(true);
    greet_row.append(&greet_spacer);
    greet_row.append(&pill);

    let grid = gtk::Grid::new();
    grid.set_column_spacing(12);
    grid.set_row_spacing(12);
    grid.set_column_homogeneous(true);
    grid.set_hexpand(true);
    grid.set_vexpand(true);
    grid.set_valign(gtk::Align::Start);

    let screen = make_tile("screen", Some(view.borrow().screen_on));
    let terminal = make_tile("terminal", Some(view.borrow().terminal_on));
    let browsers = make_tile("browsers", None);
    let obsidian = make_tile("Obsidian", None);
    let tiles = vec![
        tile_widgets(screen, TileId::Screen),
        tile_widgets(terminal, TileId::Terminal),
        tile_widgets(browsers, TileId::Browsers),
        tile_widgets(obsidian, TileId::Obsidian),
    ];

    let (pane, facts, lead, unread, page) = make_pane();
    pane.set_size_request(PANE_WIDTH, -1);
    pane.set_hexpand(false);
    pane.set_vexpand(true);

    let split = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    split.set_hexpand(true);
    split.set_vexpand(true);
    split.append(&grid);
    split.append(&pane);

    let other = gtk::Label::new(None);
    other.add_css_class("sol-name");
    other.add_css_class("sol-ink");
    other.set_halign(gtk::Align::Start);
    other.set_valign(gtk::Align::Start);
    other.set_visible(false);

    chrome.body.append(&greet_row);
    chrome.body.append(&split);
    chrome.body.append(&other);

    let shelf_rows = fill_shelf(&chrome.shelf);
    let shelf_button = gtk::Button::from_icon_name("sidebar-show-symbolic");
    shelf_button.set_visible(false);
    chrome.header_extra.append(&shelf_button);
    let (pause_button, resume_button) = pause_controls();
    chrome.header_extra.append(&pause_button);
    chrome.header_extra.append(&resume_button);

    let ui = Rc::new(Ui {
        window: window.clone(),
        shelf: chrome.shelf.clone(),
        body: chrome.body.clone(),
        grid: grid.clone(),
        split: split.clone(),
        other: other.clone(),
        pane: pane.clone(),
        greet,
        pause_line,
        pill: pill.clone(),
        pause_button,
        resume_button,
        shelf_button: shelf_button.clone(),
        tiles,
        facts,
        lead,
        unread,
        page,
        shelf_rows,
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

    for (button, which) in &ui.shelf_rows {
        let which = *which;
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
        ui,
        selected,
    );
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
    split: gtk::Box,
    other: gtk::Label,
    pane: gtk::Box,
    greet: gtk::Label,
    pause_line: gtk::Label,
    pill: gtk::Button,
    pause_button: gtk::MenuButton,
    resume_button: gtk::Button,
    shelf_button: gtk::Button,
    tiles: Vec<TileWidgets>,
    facts: [FactWidgets; 4],
    lead: gtk::Label,
    unread: gtk::Label,
    page: Page,
    shelf_rows: Vec<(gtk::Button, ShelfRow)>,
}

struct Page {
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
        self.box_.set_visible(true);
    }
}

fn tile_widgets(
    built: (gtk::Box, gtk::Label, gtk::Label, Option<gtk::Switch>),
    id: TileId,
) -> TileWidgets {
    TileWidgets {
        root: built.0,
        word: built.1,
        sub: built.2,
        switch: built.3,
        id,
    }
}

fn make_tile(
    name: &str,
    switch_on: Option<bool>,
) -> (gtk::Box, gtk::Label, gtk::Label, Option<gtk::Switch>) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.add_css_class("sol-tile");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::new(Some(name));
    title.add_css_class("sol-name");
    title.add_css_class("sol-ink");
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    title.set_xalign(0.0);
    top.append(&title);
    let switch = switch_on.map(|on| {
        let switch = gtk::Switch::new();
        switch.set_active(on);
        switch.set_valign(gtk::Align::Center);
        top.append(&switch);
        switch
    });
    let word = gtk::Label::new(Some(shell_model::state_label(StateWord::Checking)));
    word.add_css_class("sol-faint");
    word.set_halign(gtk::Align::Start);
    word.set_xalign(0.0);
    let sub = gtk::Label::new(None);
    sub.add_css_class("sol-faint");
    sub.set_halign(gtk::Align::Start);
    sub.set_xalign(0.0);
    sub.set_wrap(true);
    sub.set_visible(false);
    root.append(&top);
    root.append(&word);
    root.append(&sub);
    (root, word, sub, switch)
}

fn make_pane() -> (gtk::Box, [FactWidgets; 4], gtk::Label, gtk::Label, Page) {
    let pane = gtk::Box::new(gtk::Orientation::Vertical, 10);
    pane.add_css_class("sol-pane");
    let lead = gtk::Label::new(Some(shell_model::state_label(StateWord::Checking)));
    lead.add_css_class("sol-name");
    lead.add_css_class("sol-ink");
    lead.set_halign(gtk::Align::Start);
    pane.append(&lead);
    let names = ["screen", "terminal", "browsers", "Obsidian"];
    let mut facts = Vec::new();
    for name in names {
        let block = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let title = gtk::Label::new(Some(name));
        title.add_css_class("sol-ink");
        title.set_halign(gtk::Align::Start);
        block.append(&title);
        let so_far_row = fact_line("so far");
        let last_row = fact_line("last taken in");
        block.append(&so_far_row.0);
        block.append(&last_row.0);
        pane.append(&block);
        facts.push(FactWidgets {
            so_far: so_far_row.1,
            last_row: last_row.0,
            last: last_row.1,
        });
    }
    let unread = gtk::Label::new(None);
    unread.add_css_class("sol-faint");
    unread.set_halign(gtk::Align::Start);
    unread.set_wrap(true);
    unread.set_visible(false);
    pane.append(&unread);

    let page_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    page_box.add_css_class("sol-page");
    page_box.set_visible(false);
    let page_top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::new(None);
    title.add_css_class("sol-name");
    title.add_css_class("sol-ink");
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    let close = gtk::Button::with_label("×");
    close.set_tooltip_text(Some("close"));
    page_top.append(&title);
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
    (
        pane,
        facts,
        lead,
        unread,
        Page {
            box_: page_box,
            title,
            word,
            reason,
            close,
            so_far,
            last,
        },
    )
}

fn fact_line(name: &str) -> (gtk::Box, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let name = gtk::Label::new(Some(name));
    name.add_css_class("sol-faint");
    name.set_halign(gtk::Align::Start);
    let value = gtk::Label::new(Some("nothing yet"));
    value.add_css_class("sol-ink");
    value.set_halign(gtk::Align::End);
    value.set_hexpand(true);
    row.append(&name);
    row.append(&value);
    (row, value)
}

fn fill_shelf(shelf: &gtk::Box) -> Vec<(gtk::Button, ShelfRow)> {
    let rows = [
        ("home", ShelfRow::Home),
        ("your journal", ShelfRow::Journal),
        ("this computer", ShelfRow::Computer),
        ("updates", ShelfRow::Updates),
        ("help", ShelfRow::Help),
    ];
    rows.into_iter()
        .map(|(label, which)| {
            let button = gtk::Button::with_label(label);
            button.add_css_class("sol-shelf-row");
            button.add_css_class("sol-ink");
            button.set_halign(gtk::Align::Fill);
            shelf.append(&button);
            (button, which)
        })
        .collect()
}

fn pause_controls() -> (gtk::MenuButton, gtk::Button) {
    let pause = gtk::MenuButton::new();
    pause.set_label("pause");
    let popover = gtk::Popover::new();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for (label, _) in shell_model::pause_menu() {
        menu.append(&gtk::Button::with_label(label));
    }
    popover.set_child(Some(&menu));
    pause.set_popover(Some(&popover));
    let resume = gtk::Button::with_label("resume");
    resume.set_visible(false);
    (pause, resume)
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
    match &view.pause {
        Some(left) => {
            ui.pause_line.set_text(&shell_model::pause_line(*left));
            ui.pause_line.set_visible(true);
            ui.pause_button.set_visible(false);
            ui.resume_button.set_visible(true);
            ui.pill.set_label(&shell_model::pause_line(*left));
        }
        None => {
            ui.pause_line.set_visible(false);
            ui.pause_button.set_visible(true);
            ui.resume_button.set_visible(false);
            ui.pill
                .set_label(shell_model::state_label(StateWord::Checking));
        }
    }
    let words = [view.screen, view.terminal, view.browsers, view.obsidian];
    let subs = [
        shell_model::screen_sub_label(view.screen_sub),
        view.terminal_reason,
        view.browsers_sub.as_deref().or(view.browsers_reason),
        None,
    ];
    for (tile, (word, sub)) in ui.tiles.iter().zip(words.into_iter().zip(subs)) {
        tile.word.set_text(shell_model::state_label(word));
        paint_word(&tile.word, word);
        match sub {
            Some(text) => {
                tile.sub.set_text(text);
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
    ui.lead
        .set_text(shell_model::state_label(if view.pause.is_some() {
            StateWord::Paused
        } else {
            StateWord::Checking
        }));
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
            Some(text) => {
                fact.last.set_text(text);
                fact.last_row.set_visible(true);
            }
            None => fact.last_row.set_visible(false),
        }
    }
    if hidden {
        ui.unread.set_text(shell_model::INTAKE_UNREADABLE);
        ui.unread.set_visible(true);
    } else {
        ui.unread.set_visible(false);
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
    place_tiles(&ui.grid, &ui.tiles, cols);
    let home = row.get() == ShelfRow::Home;
    ui.split.set_visible(home);
    ui.other.set_visible(!home);
    if !home {
        ui.other.set_text(shelf_label(row.get()));
    }
    for (button, which) in &ui.shelf_rows {
        if *which == row.get() {
            button.add_css_class("sol-selected");
        } else {
            button.remove_css_class("sol-selected");
        }
    }
    for tile in &ui.tiles {
        if selected.get() == Some(tile.id) {
            tile.root.add_css_class("sol-selected");
        } else {
            tile.root.remove_css_class("sol-selected");
        }
    }
    if phone && home {
        let open = page_open.get();
        ui.grid.set_visible(!open);
        ui.pane.set_visible(open);
    } else if home {
        ui.grid.set_visible(true);
        ui.pane.set_visible(true);
    }
}

fn place_tiles(grid: &gtk::Grid, tiles: &[TileWidgets], cols: i32) {
    let cols = cols.max(1);
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
    for (index, tile) in tiles.iter().enumerate() {
        let index = i32::try_from(index).unwrap_or(0);
        grid.attach(&tile.root, index % cols, index / cols, 1, 1);
    }
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
    let screen = if config.activity_only || !config.desktop_capture {
        shell_model::screen_tile(
            config.desktop_capture,
            config.activity_only,
            false,
            solstone_linux::tray_model::TrayStatus::Idle,
        )
    } else {
        shell_model::ScreenTile {
            word: StateWord::Checking,
            switch_on: true,
            sub: ScreenSub::None,
        }
    };
    let (terminal, terminal_reason) = terminal_word(&config, daemon_up, app_paused);
    let (browsers, browsers_reason, browsers_sub) = browsers_word(&config, seen_report);
    View {
        screen_on: config.desktop_capture,
        terminal_on: config.terminal,
        screen: screen.word,
        screen_sub: screen.sub,
        terminal,
        terminal_reason,
        browsers,
        browsers_reason,
        browsers_sub,
        obsidian: StateWord::Checking,
        intake,
        pause: paused,
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
