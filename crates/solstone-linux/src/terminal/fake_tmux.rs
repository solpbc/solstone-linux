// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! A tmux that answers from its own state, for tests that run the real engine.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use solstone_terminal::command::{CommandError, CommandInvocation, CommandOutput, CommandRunner};

/// What a fake tmux server holds: its identity and its global options.
#[derive(Clone, Debug)]
struct FakeServer {
    pub identity: String,
    pub options: std::collections::BTreeMap<String, String>,
}

#[derive(Default)]
struct FakeTmuxState {
    server: Option<FakeServer>,
    servers_started: u32,
    content: u64,
    calls: Vec<Vec<String>>,
}

/// A tmux that answers every command the engine sends from its own state: a
/// server, its options, and one session with one pane.
#[derive(Clone, Default)]
pub struct FakeTmux {
    state: Arc<Mutex<FakeTmuxState>>,
}

const NO_SERVER: &[u8] = b"no server running on /tmp/tmux-1000/default\n";
// A client activity later than any test clock, so every poll sees the client.
const CLIENT_ACTIVITY: i64 = 4_102_444_800;

impl FakeTmux {
    /// A fake with a running server holding these options.
    pub fn with_server(options: &[(&str, &str)]) -> Self {
        let fake = Self::default();
        fake.start_server(options);
        fake
    }

    /// Starts a new server, replacing any running one.
    pub fn start_server(&self, options: &[(&str, &str)]) {
        let mut state = self.lock();
        state.servers_started += 1;
        let identity = format!(
            "{} 1000{}",
            4000 + state.servers_started,
            state.servers_started
        );
        state.server = Some(FakeServer {
            identity,
            options: options
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        });
    }

    pub fn option(&self, name: &str) -> Option<String> {
        self.lock()
            .server
            .as_ref()
            .and_then(|server| server.options.get(name).cloned())
    }

    /// Makes the next captures differ, so each poll appends a frame.
    pub fn change_content(&self) {
        self.lock().content += 1;
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        self.lock().calls.clone()
    }

    pub fn polls(&self) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|args| {
                args.first()
                    .is_some_and(|command| command == "list-clients")
            })
            .count()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeTmuxState> {
        self.state.lock().expect("fake tmux poisoned")
    }

    fn answer(&self, args: &[String]) -> CommandOutput {
        let mut state = self.lock();
        state.calls.push(args.to_vec());
        let content = state.content;
        let Some(server) = state.server.as_mut() else {
            return CommandOutput {
                stdout: Vec::new(),
                stderr: NO_SERVER.to_vec(),
                status: 1,
            };
        };
        let strs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let ok = |stdout: String| CommandOutput {
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
            status: 0,
        };
        match strs.as_slice() {
            ["display-message", "-p", "#{pid} #{start_time}"] => {
                ok(format!("{}\n", server.identity))
            }
            ["show-options", "-gv", name] => match server.options.get(*name) {
                Some(value) => ok(format!("{value}\n")),
                None => CommandOutput {
                    stdout: Vec::new(),
                    stderr: format!("invalid option: {name}\n").into_bytes(),
                    status: 1,
                },
            },
            ["set-option", "-g", name, value] => {
                server
                    .options
                    .insert((*name).to_owned(), (*value).to_owned());
                ok(String::new())
            }
            ["set-option", "-gu", name] => {
                server.options.remove(*name);
                ok(String::new())
            }
            ["list-clients", ..] => ok(format!("main {CLIENT_ACTIVITY}\n")),
            ["list-windows", ..] => ok("1 @0 0 shell\n".to_owned()),
            ["list-panes", ..] => ok("%0 0 0 0 80 24 1 0 0 1 0 0\n".to_owned()),
            ["capture-pane", ..] => ok(format!("fake pane content {content}\n")),
            ["display-message", "-p", "-t", _, "#{pane_title}"] => ok("editor\n".to_owned()),
            ["display-message", "-p", "-t", _, "#{pane_current_path}"] => {
                ok("/home/user/project\n".to_owned())
            }
            ["display-message", "-p", "-t", _, "#{pane_pid}"] => ok("4242\n".to_owned()),
            _ => CommandOutput {
                stdout: Vec::new(),
                stderr: format!("fake tmux does not know {args:?}\n").into_bytes(),
                status: 1,
            },
        }
    }
}

impl CommandRunner for FakeTmux {
    fn run<'a>(
        &'a self,
        invocation: CommandInvocation,
    ) -> Pin<Box<dyn Future<Output = Result<CommandOutput, CommandError>> + Send + 'a>> {
        Box::pin(async move {
            let args = invocation
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            Ok(self.answer(&args))
        })
    }
}

pub struct UtcZoneSource;

impl solstone_terminal::clock::ZoneSource for UtcZoneSource {
    fn read(&mut self) -> Result<solstone_terminal::clock::Zone, String> {
        Ok(solstone_terminal::clock::Zone::utc())
    }
}
