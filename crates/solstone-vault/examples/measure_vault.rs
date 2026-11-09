// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Measures a scan of a generated vault: `cargo run --release --example
//! measure_vault -p solstone-vault -- <dir> <notes>`. Prints wall time, peak
//! resident memory and the table's size. Writes only under `<dir>`.

use std::{
    fs,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant, SystemTime},
};

use solstone_vault::{Engine, ScanOutcome, SystemClock};

fn peak_rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmHWM:")
                    .and_then(|rest| rest.trim().trim_end_matches(" kB").parse().ok())
            })
        })
        .unwrap_or(0)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("usage: measure_vault <dir> <notes>");
    let notes: usize = args
        .next()
        .expect("usage: measure_vault <dir> <notes>")
        .parse()
        .unwrap();
    let root = Path::new(&root);
    let vault = root.join("Measure");
    let state = root.join("state");
    fs::create_dir_all(vault.join(".obsidian")).unwrap();
    // Realistically sized notes: a kilobyte or two across a few hundred folders.
    let paragraph = "A paragraph of plain writing that says nothing in particular. ".repeat(20);
    let day_ago = SystemTime::now() - Duration::from_secs(86_400);
    let start = Instant::now();
    for index in 0..notes {
        let dir = vault.join(format!("folder{:03}", index % 300));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("note {index}.md"));
        fs::write(&path, format!("# note {index}\n\n{paragraph}\n")).unwrap();
        // Spread over a day, so windows are closed already and each one is its own file.
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(day_ago + Duration::from_secs((index as u64 % 288) * 300))
            .unwrap();
    }
    println!("generated {notes} notes in {:.1?}", start.elapsed());

    let mut engine = Engine::open(state.clone(), Arc::new(SystemClock)).unwrap();
    engine.turn_on(&vault).unwrap();
    engine.apply_vault_list(&[]).unwrap();
    let cancel = AtomicBool::new(false);

    let start = Instant::now();
    assert_eq!(engine.scan(&cancel).unwrap(), ScanOutcome::Done);
    println!(
        "first scan (read and hash everything): {:.2?}, peak RSS {} MiB",
        start.elapsed(),
        peak_rss_kib() / 1024
    );

    let start = Instant::now();
    assert_eq!(engine.scan(&cancel).unwrap(), ScanOutcome::Done);
    println!(
        "repeat scan (stat walk, nothing changed): {:.2?}, peak RSS {} MiB",
        start.elapsed(),
        peak_rss_kib() / 1024
    );

    let start = Instant::now();
    let mut windows = 0;
    loop {
        let closed = engine.close_due_windows(500).unwrap();
        if closed == 0 {
            break;
        }
        windows += closed;
    }
    println!(
        "closed {windows} windows into outbox files: {:.2?}, peak RSS {} MiB",
        start.elapsed(),
        peak_rss_kib() / 1024
    );

    let start = Instant::now();
    let mut files = 0;
    while let Some(delivery) = engine.next_delivery() {
        engine
            .record_answer(&delivery, solstone_vault::Answer::Delivered)
            .unwrap();
        files += 1;
    }
    println!(
        "drained {files} outbox files: {:.2?}, peak RSS {} MiB",
        start.elapsed(),
        peak_rss_kib() / 1024
    );

    let table = fs::read_dir(state.join("tables"))
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    println!(
        "table on disk: {} KiB, pending.log: {} KiB",
        table.metadata().unwrap().len() / 1024,
        fs::metadata(state.join("pending.log")).unwrap().len() / 1024
    );
}
