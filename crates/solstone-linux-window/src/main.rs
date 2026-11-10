// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

mod controller;
mod copy_map;
mod format;
mod logs;
mod mark_card;
mod sun_bg;
mod ui;

use gtk::gio::prelude::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("solstone-linux-window {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    if gtk::major_version() < 4 || (gtk::major_version() == 4 && gtk::minor_version() < 6) {
        std::process::exit(78);
    }

    if let Ok(path) = std::env::var("SOLSTONE_FRAME_PNG")
        && !path.is_empty()
    {
        match std::fs::write(format!("{path}.txt"), "main\n") {
            Ok(()) => eprintln!("frame-file armed"),
            Err(error) => eprintln!("frame-file note: {error}"),
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(70);
        });
    let _runtime_context = runtime.enter();

    // A second launch hands its arguments to the window already open, so the panel's
    // "open settings" can bring that window to its settings tab.
    let app = gtk::Application::builder()
        .application_id("org.solpbc.solstone.Window")
        .flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_activate(ui::build_ui);
    app.connect_command_line(|app, command_line| {
        app.activate();
        if command_line
            .arguments()
            .iter()
            .skip(1)
            .any(|arg| arg.to_str() == Some(solstone_linux::cli::WINDOW_SETTINGS_ARG))
        {
            ui::show_settings(app);
        }
        gtk::glib::ExitCode::SUCCESS
    });
    std::process::exit(app.run().into());
}
