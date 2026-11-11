// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

mod controller;
mod copy_map;
mod deck;
mod format;
mod logs;
mod mark_card;
mod sun_bg;
mod ui;

use gtk::gio::prelude::*;

fn main() {
    // Comfortaa is the display face for names and headings. Fontconfig reads
    // this directory on its own; the process does not set an environment variable.
    install_display_face();

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

/// The bundled face is Comfortaa, weight 700 (the name table still says Regular).
fn install_display_face() {
    let Some(dir) = display_face_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("Comfortaa-Bold.ttf");
    let bytes: &[u8] = include_bytes!("../assets/Comfortaa-Bold.ttf");
    if std::fs::read(&path).ok().as_deref() != Some(bytes) {
        let _ = std::fs::write(path, bytes);
    }
}

fn display_face_dir() -> Option<std::path::PathBuf> {
    if let Some(data) = std::env::var_os("XDG_DATA_HOME")
        && !data.is_empty()
    {
        return Some(std::path::PathBuf::from(data).join("fonts"));
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(home).join(".local/share/fonts"))
}
