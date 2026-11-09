// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Which desktop session capture follows.
//!
//! The policy: capture only a graphical session that belongs to this user and is the
//! active session on its seat, as logind reports it right now. Anything else (another
//! user in front of the seat, the greeter, a session that is only remote, or no session
//! at all) means capture waits.
//!
//! The answer is read from logind at capture time, never from a login-time snapshot.
//! The user manager's `DISPLAY` and `XAUTHORITY` belong to whichever session last ran
//! an `import-environment`, and older installs still run one at every login, so neither
//! is trusted to name the session in front of the seat.
//!
//! On X11 the session's credential is resolved the same way: from the session's own
//! processes, then this user's other credential files, then a private copy kept in the
//! runtime directory. Every candidate is proven against the session's X server before it
//! is used, and the one that works is written to the private file that this process's
//! `XAUTHORITY` names for its whole life, so a credential deleted under a live session
//! does not take capture with it.

use std::{
    collections::HashMap,
    fs, io,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// How long a proven X11 credential is trusted before it is proven again.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(60);
/// How long a live answer survives logind not answering, so one slow call does not
/// split a segment.
pub const LOGIND_GRACE: Duration = Duration::from_secs(30);
const LOGIND_TIMEOUT: Duration = Duration::from_secs(2);
const MANAGED_DIRECTORY: &str = "solstone-linux";
const MANAGED_FILENAME: &str = "Xauthority";
const FAMILY_WILD: u16 = 65535;

/// Whether capture has a live session, and if not, why.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionPresence {
    #[default]
    Live,
    /// This user has a desktop session here, but it is not the one in front of its seat.
    NotActive,
    /// This user has no local desktop session on a seat: logged out, or remote only.
    NoDesktop,
    /// This user's active desktop session can't be reached: its display or its
    /// credential is gone.
    Unreachable,
    /// logind could not be read, so no session can be confirmed.
    LoginUnavailable,
}

impl SessionPresence {
    pub fn is_live(self) -> bool {
        self == Self::Live
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopKind {
    X11,
    Wayland,
}

/// One logind session, as `org.freedesktop.login1.Session` describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginSession {
    pub id: String,
    pub path: String,
    pub uid: u32,
    pub kind: String,
    pub class: String,
    pub active: bool,
    pub remote: bool,
    pub seat: String,
    pub display: String,
    pub desktop: String,
    pub state: String,
}

impl LoginSession {
    fn desktop_kind(&self) -> Option<DesktopKind> {
        match self.kind.as_str() {
            "x11" => Some(DesktopKind::X11),
            "wayland" => Some(DesktopKind::Wayland),
            _ => None,
        }
    }

    fn is_own_desktop(&self, uid: u32) -> bool {
        self.uid == uid && self.class == "user" && self.desktop_kind().is_some()
    }

    fn is_local(&self) -> bool {
        !self.remote && !self.seat.is_empty()
    }
}

/// The session capture follows. `Err` names why there is none.
pub fn select(sessions: &[LoginSession], uid: u32) -> Result<&LoginSession, SessionPresence> {
    let own: Vec<_> = sessions.iter().filter(|s| s.is_own_desktop(uid)).collect();
    let mut eligible: Vec<_> = own
        .iter()
        .copied()
        .filter(|s| s.is_local() && s.active && s.state == "active")
        .collect();
    // logind keeps at most one active session per seat; with more than one seat, the
    // first seat wins, then the lowest session id, so the answer is stable.
    eligible.sort_by(|a, b| {
        (a.seat != "seat0", &a.seat, a.id.len(), &a.id).cmp(&(
            b.seat != "seat0",
            &b.seat,
            b.id.len(),
            &b.id,
        ))
    });
    if let Some(session) = eligible.first() {
        return Ok(session);
    }
    if own.iter().any(|s| s.is_local() && s.state != "closing") {
        Err(SessionPresence::NotActive)
    } else {
        Err(SessionPresence::NoDesktop)
    }
}

/// What capture is pointed at right now. Published by the resolver, read by the
/// video backend at every start and by health.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureTarget {
    pub presence: SessionPresence,
    pub session_id: Option<String>,
    pub session_path: Option<String>,
    pub kind: Option<DesktopKind>,
    pub desktop: String,
    /// The X display: the session's own on X11, Xwayland's (when one is found) on Wayland.
    pub display: Option<String>,
    /// Changes whenever the session, its kind, its display or its presence changes.
    pub epoch: u64,
    /// This user's local desktop sessions, live or not. Above one, the user bus's screen
    /// saver can't say which session it speaks for.
    pub local_desktops: usize,
    pub detail: String,
}

impl CaptureTarget {
    fn identity(
        &self,
    ) -> (
        SessionPresence,
        Option<&str>,
        Option<DesktopKind>,
        Option<&str>,
        &str,
    ) {
        (
            self.presence,
            self.session_id.as_deref(),
            self.kind,
            self.display.as_deref(),
            &self.desktop,
        )
    }
}

pub type SharedTarget = Arc<Mutex<CaptureTarget>>;

pub trait LoginSessions {
    /// Every session logind knows for `uid`.
    fn list(&mut self, uid: u32) -> Result<Vec<LoginSession>, String>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRouteEnv {
    pub desktop: Option<String>,
    pub sway_socket: Option<String>,
}

pub trait SessionProcesses {
    /// The `DISPLAY`, `XAUTHORITY` and `WAYLAND_DISPLAY` of each of this user's
    /// processes inside the session's scope.
    fn environments(&mut self, session_id: &str) -> Vec<HashMap<String, String>>;

    fn route_environment(&mut self, session_id: &str) -> SessionRouteEnv {
        let _ = session_id;
        SessionRouteEnv::default()
    }
}

/// An X11 authorization: protocol name and data. Empty is "no credential".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct XCredential {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
}

pub trait XServer {
    /// Opens and closes one connection to `display` with exactly `credential`.
    fn connect(&mut self, display: &str, credential: &XCredential) -> Result<(), String>;
}

/// One `.Xauthority` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XauthEntry {
    pub family: u16,
    pub address: Vec<u8>,
    pub number: Vec<u8>,
    pub name: Vec<u8>,
    pub data: Vec<u8>,
}

/// Parses an `.Xauthority` file, keeping every whole entry before any truncation.
pub fn parse_xauthority(bytes: &[u8]) -> Vec<XauthEntry> {
    fn field<'a>(bytes: &'a [u8], at: &mut usize) -> Option<&'a [u8]> {
        let length = usize::from(u16::from_be_bytes(
            bytes.get(*at..*at + 2)?.try_into().ok()?,
        ));
        let value = bytes.get(*at + 2..*at + 2 + length)?;
        *at += 2 + length;
        Some(value)
    }
    let mut entries = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let Some(family) = bytes
            .get(at..at + 2)
            .map(|value| u16::from_be_bytes([value[0], value[1]]))
        else {
            break;
        };
        let mut cursor = at + 2;
        let parsed = (|| {
            Some(XauthEntry {
                family,
                address: field(bytes, &mut cursor)?.to_vec(),
                number: field(bytes, &mut cursor)?.to_vec(),
                name: field(bytes, &mut cursor)?.to_vec(),
                data: field(bytes, &mut cursor)?.to_vec(),
            })
        })();
        let Some(entry) = parsed else { break };
        entries.push(entry);
        at = cursor;
    }
    entries
}

pub fn encode_xauthority(entries: &[XauthEntry]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in entries {
        bytes.extend_from_slice(&entry.family.to_be_bytes());
        for value in [&entry.address, &entry.number, &entry.name, &entry.data] {
            let length = u16::try_from(value.len()).unwrap_or(u16::MAX);
            bytes.extend_from_slice(&length.to_be_bytes());
            bytes.extend_from_slice(&value[..usize::from(length)]);
        }
    }
    bytes
}

/// The display number in `:1`, `:1.0` or `unix:1`.
pub fn display_number(display: &str) -> Option<u16> {
    x11rb::reexports::x11rb_protocol::parse_display::parse_display(Some(display))
        .ok()
        .map(|parsed| parsed.display)
}

fn credentials_for_display(bytes: &[u8], number: u16) -> Vec<XCredential> {
    let number = number.to_string();
    parse_xauthority(bytes)
        .into_iter()
        .filter(|entry| entry.number.is_empty() || entry.number == number.as_bytes())
        .map(|entry| XCredential {
            name: entry.name,
            data: entry.data,
        })
        .collect()
}

/// The private `XAUTHORITY` this process names for its whole life.
pub fn managed_xauthority_path(runtime_dir: Option<&str>, uid: u32) -> PathBuf {
    let runtime = runtime_dir
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
    runtime.join(MANAGED_DIRECTORY).join(MANAGED_FILENAME)
}

/// What the process was started with, kept as credential and display fallbacks.
#[derive(Clone, Debug, Default)]
pub struct StartupEnvironment {
    pub display: Option<String>,
    pub wayland_display: Option<String>,
    pub xauthority: Option<String>,
    pub home: Option<PathBuf>,
}

static STARTUP: std::sync::RwLock<Option<StartupEnvironment>> = std::sync::RwLock::new(None);

/// Called once, before `XAUTHORITY` is pointed at the private file.
pub fn remember_startup_environment(environment: StartupEnvironment) {
    let mut slot = STARTUP
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.is_none() {
        *slot = Some(environment);
    }
}

/// Replaces the remembered session environment with what the user manager holds
/// now, for an app that started before any desktop session existed. Values the
/// manager does not have are kept.
pub fn refresh_startup_environment(manager: &std::collections::HashMap<String, String>) {
    let mut slot = STARTUP
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current = slot.get_or_insert_with(StartupEnvironment::default);
    if let Some(display) = manager.get("DISPLAY").filter(|value| !value.is_empty()) {
        current.display = Some(display.clone());
    }
    if let Some(wayland) = manager
        .get("WAYLAND_DISPLAY")
        .filter(|value| !value.is_empty())
    {
        current.wayland_display = Some(wayland.clone());
    }
    if let Some(xauthority) = manager.get("XAUTHORITY").filter(|value| !value.is_empty()) {
        current.xauthority = Some(xauthority.clone());
    }
}

/// Reads the user manager's environment into the remembered one. The desktop
/// imports its variables there as it starts, which can be after this app did.
pub fn reread_manager_environment() {
    let manager = crate::session_env::manager_environment(&crate::cli::SystemRunner);
    if !manager.is_empty() {
        refresh_startup_environment(&manager);
    }
}

/// The display variables a program this app launches needs. An app that started
/// before the desktop did was not given them in its own environment.
pub fn display_environment_for_children() -> Vec<(&'static str, String)> {
    let environment = startup_environment();
    [
        ("DISPLAY", environment.display),
        ("WAYLAND_DISPLAY", environment.wayland_display),
        ("XAUTHORITY", environment.xauthority),
    ]
    .into_iter()
    .filter_map(|(name, value)| Some((name, value.filter(|value| !value.is_empty())?)))
    .collect()
}

pub fn startup_environment() -> StartupEnvironment {
    STARTUP
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_default()
}

/// The last live resolution of a session, reused until [`RECHECK_INTERVAL`] passes.
struct Proven {
    session_id: String,
    display: Option<String>,
    credential: Option<XCredential>,
    at: Instant,
}

pub struct Resolver<L, P, X> {
    logind: L,
    processes: P,
    xserver: X,
    uid: u32,
    managed: PathBuf,
    fallback_files: Vec<PathBuf>,
    fallback_display: Option<String>,
    proven: Option<Proven>,
    last_answer: Option<Instant>,
    current: CaptureTarget,
    read_only: bool,
    text_launched: HashMap<String, (Option<String>, Instant)>,
}

impl<L: LoginSessions, P: SessionProcesses, X: XServer> Resolver<L, P, X> {
    pub fn new(
        logind: L,
        processes: P,
        xserver: X,
        uid: u32,
        managed: PathBuf,
        startup: StartupEnvironment,
    ) -> Self {
        let mut fallback_files = Vec::new();
        if let Some(path) = startup.xauthority.filter(|value| !value.is_empty()) {
            fallback_files.push(PathBuf::from(path));
        }
        if let Some(home) = startup.home {
            fallback_files.push(home.join(".Xauthority"));
        }
        Self {
            logind,
            processes,
            xserver,
            uid,
            managed,
            fallback_files,
            fallback_display: startup.display.filter(|value| !value.is_empty()),
            proven: None,
            last_answer: None,
            read_only: false,
            text_launched: HashMap::new(),
            current: CaptureTarget {
                presence: SessionPresence::LoginUnavailable,
                detail: "no session read yet".into(),
                ..CaptureTarget::default()
            },
        }
    }

    pub fn current(&self) -> &CaptureTarget {
        &self.current
    }

    /// Takes the session environment the user manager holds now, for an app that
    /// started before any desktop session existed.
    pub fn adopt_startup(&mut self, startup: StartupEnvironment) {
        let mut fallback_files = Vec::new();
        if let Some(path) = startup.xauthority.filter(|value| !value.is_empty()) {
            fallback_files.push(PathBuf::from(path));
        }
        if let Some(home) = startup.home {
            fallback_files.push(home.join(".Xauthority"));
        }
        self.fallback_files = fallback_files;
        self.fallback_display = startup.display.filter(|value| !value.is_empty());
    }

    pub fn route_environment(&mut self, session_id: &str) -> SessionRouteEnv {
        if self.current.session_id.as_deref() != Some(session_id)
            || !self.current.presence.is_live()
        {
            return SessionRouteEnv::default();
        }
        let mut env = self.processes.route_environment(session_id);
        // Desktop and eligibility come from the same logind selection. A second
        // selection here could describe a different seat/session snapshot.
        {
            let desktop = if self.current.desktop.eq_ignore_ascii_case("plasma") {
                "KDE"
            } else {
                &self.current.desktop
            };
            if ["KDE", "GNOME", "sway"]
                .iter()
                .any(|token| crate::activity::desktop_has_token(desktop, token))
            {
                env.desktop = Some(desktop.to_owned());
            }
        }
        env
    }

    pub fn resolve(&mut self, now: Instant) -> CaptureTarget {
        let next = match self.logind.list(self.uid) {
            Ok(sessions) => {
                self.last_answer = Some(now);
                self.target_for(&sessions, now)
            }
            Err(error) => {
                let within_grace = self
                    .last_answer
                    .is_some_and(|at| now.saturating_duration_since(at) < LOGIND_GRACE);
                if within_grace {
                    return self.current.clone();
                }
                absent(
                    SessionPresence::LoginUnavailable,
                    format!("could not read login sessions: {error}"),
                    0,
                )
            }
        };
        self.adopt(next)
    }

    fn adopt(&mut self, mut next: CaptureTarget) -> CaptureTarget {
        next.epoch = if next.identity() == self.current.identity() {
            self.current.epoch
        } else {
            self.current.epoch.wrapping_add(1)
        };
        if !self.read_only && next.identity() != self.current.identity() {
            tracing::info!(
                presence = ?next.presence,
                session = next.session_id.as_deref().unwrap_or("-"),
                kind = ?next.kind,
                display = next.display.as_deref().unwrap_or("-"),
                "capture session: {}",
                next.detail
            );
        }
        self.current = next;
        self.current.clone()
    }

    /// A compositor or `startx` launched from a text login leaves logind saying `tty`.
    /// When this user's own active local text session runs a display server, it is a
    /// desktop session in front of the seat, and is treated as one.
    fn with_text_launched_desktops(
        &mut self,
        sessions: &[LoginSession],
        now: Instant,
    ) -> Vec<LoginSession> {
        let mut sessions = sessions.to_vec();
        for session in &mut sessions {
            let text_login = matches!(session.kind.as_str(), "tty" | "unspecified");
            if !(text_login
                && session.uid == self.uid
                && session.class == "user"
                && session.active
                && session.is_local())
            {
                continue;
            }
            let cached = self
                .text_launched
                .get(&session.id)
                .filter(|(_, at)| now.saturating_duration_since(*at) < RECHECK_INTERVAL)
                .map(|(kind, _)| kind.clone());
            let kind = cached.unwrap_or_else(|| {
                let environments = self.processes.environments(&session.id);
                let kind = if first_value(&environments, "WAYLAND_DISPLAY").is_some() {
                    Some("wayland".to_owned())
                } else if first_value(&environments, "DISPLAY").is_some() {
                    Some("x11".to_owned())
                } else {
                    None
                };
                self.text_launched
                    .insert(session.id.clone(), (kind.clone(), now));
                kind
            });
            if let Some(kind) = kind {
                session.kind = kind;
            }
        }
        sessions
    }

    fn target_for(&mut self, sessions: &[LoginSession], now: Instant) -> CaptureTarget {
        let sessions = &self.with_text_launched_desktops(sessions, now)[..];
        let local_desktops = sessions
            .iter()
            .filter(|s| s.is_own_desktop(self.uid) && s.is_local() && s.state != "closing")
            .count();
        let session = match select(sessions, self.uid) {
            Ok(session) => session.clone(),
            Err(presence) => {
                let detail = match presence {
                    SessionPresence::NotActive => {
                        "this user's session is not the one in front of the seat".to_owned()
                    }
                    _ => "no desktop session of this user on a seat".to_owned(),
                };
                return absent(presence, detail, local_desktops);
            }
        };
        let kind = session
            .desktop_kind()
            .expect("selected sessions are graphical");
        let mut target = CaptureTarget {
            presence: SessionPresence::Live,
            session_id: Some(session.id.clone()),
            session_path: Some(session.path.clone()),
            kind: Some(kind),
            desktop: session.desktop.clone(),
            display: None,
            epoch: 0,
            local_desktops,
            detail: format!("session {} ({})", session.id, session.kind),
        };
        let reuse = self.proven.as_ref().filter(|proven| {
            proven.session_id == session.id
                && now.saturating_duration_since(proven.at) < RECHECK_INTERVAL
        });
        if let Some(proven) = reuse {
            target.display = proven.display.clone();
            return target;
        }
        let environments = self.processes.environments(&session.id);
        let display = match kind {
            DesktopKind::X11 => Some(session.display.clone())
                .filter(|value| !value.is_empty())
                .or_else(|| first_value(&environments, "DISPLAY")),
            DesktopKind::Wayland => {
                first_value(&environments, "DISPLAY").or_else(|| self.fallback_display.clone())
            }
        };
        let Some(display) = display else {
            if kind == DesktopKind::X11 {
                target.presence = SessionPresence::Unreachable;
                target.detail = format!("session {} names no X display", session.id);
            } else {
                self.remember(&session.id, None, None, now);
            }
            return target;
        };
        target.display = Some(display.clone());
        match self.prove_credential(&display, &environments) {
            Ok(credential) => self.remember(&session.id, Some(display), Some(credential), now),
            Err(error) if kind == DesktopKind::X11 => {
                target.presence = SessionPresence::Unreachable;
                target.detail = format!("session {} on {display}: {error}", session.id);
            }
            // Wayland capture never uses X; Xwayland's credential only serves the
            // idle and power probes.
            Err(_) => self.remember(&session.id, Some(display), None, now),
        }
        target
    }

    fn remember(
        &mut self,
        session_id: &str,
        display: Option<String>,
        credential: Option<XCredential>,
        now: Instant,
    ) {
        self.proven = Some(Proven {
            session_id: session_id.to_owned(),
            display,
            credential,
            at: now,
        });
    }

    fn prove_credential(
        &mut self,
        display: &str,
        environments: &[HashMap<String, String>],
    ) -> Result<XCredential, String> {
        let number =
            display_number(display).ok_or_else(|| format!("unreadable display {display}"))?;
        let mut candidates = Vec::new();
        if let Some(credential) = self
            .proven
            .as_ref()
            .filter(|p| p.display.as_deref() == Some(display))
            .and_then(|p| p.credential.clone())
        {
            candidates.push(credential);
        }
        let mut files: Vec<PathBuf> = environments
            .iter()
            .filter_map(|environment| environment.get("XAUTHORITY"))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect();
        files.extend(self.fallback_files.iter().cloned());
        files.push(self.managed.clone());
        for file in files {
            if let Ok(bytes) = fs::read(&file) {
                candidates.extend(credentials_for_display(&bytes, number));
            }
        }
        // Last: a server that admits this user without a cookie (server-interpreted
        // local-user access).
        candidates.push(XCredential::default());
        let mut tried = Vec::new();
        let mut last_error = "no credential".to_owned();
        for candidate in candidates {
            if tried.contains(&candidate) {
                continue;
            }
            match self.xserver.connect(display, &candidate) {
                Ok(()) => {
                    if self.read_only {
                        return Ok(candidate);
                    }
                    let seed = self.fallback_files.first().map(PathBuf::as_path);
                    if let Err(error) = write_managed(&self.managed, seed, number, &candidate) {
                        tracing::warn!(%error, path = %self.managed.display(), "could not keep the X11 credential");
                    }
                    return Ok(candidate);
                }
                Err(error) => last_error = error,
            }
            tried.push(candidate);
        }
        let reason = last_error
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        Err(format!("X11 authorization unavailable ({reason})"))
    }
}

fn first_value(environments: &[HashMap<String, String>], name: &str) -> Option<String> {
    environments
        .iter()
        .filter_map(|environment| environment.get(name))
        .find(|value| !value.is_empty())
        .cloned()
}

fn absent(presence: SessionPresence, detail: String, local_desktops: usize) -> CaptureTarget {
    CaptureTarget {
        presence,
        local_desktops,
        detail,
        ..CaptureTarget::default()
    }
}

/// Puts the proven credential for display `number` first and keeps every other
/// display's entries, starting from the startup file the first time, so a child process
/// that still names another display keeps the access it was started with.
fn write_managed(
    path: &Path,
    seed: Option<&Path>,
    number: u16,
    credential: &XCredential,
) -> io::Result<()> {
    let wanted = number.to_string().into_bytes();
    let base = fs::read(path)
        .ok()
        .or_else(|| seed.and_then(|seed| fs::read(seed).ok()))
        .unwrap_or_default();
    let mut entries = Vec::new();
    if !credential.name.is_empty() {
        entries.push(XauthEntry {
            family: FAMILY_WILD,
            address: Vec::new(),
            number: wanted.clone(),
            name: credential.name.clone(),
            data: credential.data.clone(),
        });
    }
    entries.extend(
        parse_xauthority(&base)
            .into_iter()
            .filter(|entry| !entry.number.is_empty() && entry.number != wanted),
    );
    let bytes = encode_xauthority(&entries);
    if fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("credential path has no parent"))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    let temporary = directory.join(format!(".{MANAGED_FILENAME}.{}", std::process::id()));
    {
        use io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
}

/// logind over the system bus.
pub struct NativeLogind {
    runtime: tokio::runtime::Runtime,
    connection: Option<zbus::Connection>,
}

impl NativeLogind {
    pub fn new() -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self {
            runtime,
            connection: None,
        })
    }
}

impl LoginSessions for NativeLogind {
    fn list(&mut self, uid: u32) -> Result<Vec<LoginSession>, String> {
        let cached = self.connection.clone();
        let result = self.runtime.block_on(async {
            tokio::time::timeout(LOGIND_TIMEOUT, async {
                let connection = match cached {
                    Some(connection) => connection,
                    None => zbus::Connection::system().await?,
                };
                let sessions = list_sessions(&connection, uid).await?;
                Ok::<_, zbus::Error>((connection, sessions))
            })
            .await
        });
        match result {
            Ok(Ok((connection, sessions))) => {
                self.connection = Some(connection);
                Ok(sessions)
            }
            Ok(Err(error)) => {
                self.connection = None;
                Err(error.to_string())
            }
            Err(_) => {
                self.connection = None;
                Err(format!("timed out after {}s", LOGIND_TIMEOUT.as_secs()))
            }
        }
    }
}

async fn list_sessions(connection: &zbus::Connection, uid: u32) -> zbus::Result<Vec<LoginSession>> {
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};
    let manager = zbus::Proxy::new(
        connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let listed: Vec<(String, u32, String, String, OwnedObjectPath)> =
        manager.call("ListSessions", &()).await?;
    let interface = zbus::names::InterfaceName::try_from("org.freedesktop.login1.Session")?;
    let mut sessions = Vec::new();
    for (id, session_uid, _user, seat, path) in listed {
        if session_uid != uid {
            continue;
        }
        let properties = zbus::fdo::PropertiesProxy::builder(connection)
            .destination("org.freedesktop.login1")?
            .path(path.clone())?
            .build()
            .await?;
        // A session can close between the listing and this read; skip it.
        let Ok(values) = properties.get_all(interface.clone()).await else {
            continue;
        };
        let text = |name: &str| -> String {
            values
                .get(name)
                .and_then(|value| String::try_from(value.clone()).ok())
                .unwrap_or_default()
        };
        let flag = |name: &str| -> bool {
            values
                .get(name)
                .and_then(|value: &OwnedValue| bool::try_from(value).ok())
                .unwrap_or(false)
        };
        let seat_id = values
            .get("Seat")
            .and_then(|value| <(String, OwnedObjectPath)>::try_from(value.clone()).ok())
            .map(|(seat, _)| seat)
            .unwrap_or(seat);
        sessions.push(LoginSession {
            id,
            path: path.to_string(),
            uid: session_uid,
            kind: text("Type"),
            class: text("Class"),
            active: flag("Active"),
            remote: flag("Remote"),
            seat: seat_id,
            display: text("Display"),
            desktop: text("Desktop"),
            state: text("State"),
        });
    }
    Ok(sessions)
}

/// This user's processes in a session's scope, read from `/proc`.
pub struct ProcSessionProcesses {
    root: PathBuf,
    uid: u32,
}

impl ProcSessionProcesses {
    pub fn new(uid: u32) -> Self {
        Self::with_root("/proc", uid)
    }
    pub fn with_root(root: impl Into<PathBuf>, uid: u32) -> Self {
        Self {
            root: root.into(),
            uid,
        }
    }
}

const KEPT: [&str; 3] = ["DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY"];

impl SessionProcesses for ProcSessionProcesses {
    fn environments(&mut self, session_id: &str) -> Vec<HashMap<String, String>> {
        let scope = format!("/session-{session_id}.scope");
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let directory = entry.path();
            if fs::metadata(&directory).map(|m| m.uid()).ok() != Some(self.uid) {
                continue;
            }
            let in_scope = fs::read_to_string(directory.join("cgroup")).is_ok_and(|cgroup| {
                cgroup.lines().any(|line| {
                    line.rsplit(':')
                        .next()
                        .is_some_and(|path| path.split('/').any(|part| format!("/{part}") == scope))
                })
            });
            if !in_scope {
                continue;
            }
            let Ok(environ) = fs::read(directory.join("environ")) else {
                continue;
            };
            let kept: HashMap<String, String> = environ
                .split(|byte| *byte == 0)
                .filter_map(|pair| {
                    let pair = std::str::from_utf8(pair).ok()?;
                    let (key, value) = pair.split_once('=')?;
                    KEPT.contains(&key)
                        .then(|| (key.to_owned(), value.to_owned()))
                })
                .collect();
            if !kept.is_empty() {
                found.push(kept);
            }
        }
        found
    }

    fn route_environment(&mut self, session_id: &str) -> SessionRouteEnv {
        let scope = format!("/session-{session_id}.scope");
        let Ok(entries) = fs::read_dir(&self.root) else {
            return SessionRouteEnv::default();
        };
        let mut seen_desktop: Option<String> = None;
        let mut desktop_disagree = false;
        let mut seen_sway_socket: Option<String> = None;
        let mut sway_disagree = false;
        let mut compositor_sockets = Vec::new();

        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let directory = entry.path();
            if fs::metadata(&directory).map(|m| m.uid()).ok() != Some(self.uid) {
                continue;
            }
            let in_scope = fs::read_to_string(directory.join("cgroup")).is_ok_and(|cgroup| {
                cgroup.lines().any(|line| {
                    line.rsplit(':')
                        .next()
                        .is_some_and(|path| path.split('/').any(|part| format!("/{part}") == scope))
                })
            });
            if !in_scope {
                continue;
            }
            let Ok(environ) = fs::read(directory.join("environ")) else {
                continue;
            };
            let mut runtime_dir = format!("/run/user/{}", self.uid);
            for pair in environ.split(|byte| *byte == 0) {
                let Ok(pair) = std::str::from_utf8(pair) else {
                    continue;
                };
                let Some((key, value)) = pair.split_once('=') else {
                    continue;
                };
                if key == "XDG_RUNTIME_DIR" && value.starts_with('/') {
                    runtime_dir = value.to_owned();
                } else if key == "XDG_CURRENT_DESKTOP" && !value.is_empty() {
                    if let Some(existing) = &seen_desktop {
                        if existing != value {
                            desktop_disagree = true;
                        }
                    } else {
                        seen_desktop = Some(value.to_owned());
                    }
                } else if key == "SWAYSOCK" && !value.is_empty() {
                    if let Some(existing) = &seen_sway_socket {
                        if existing != value {
                            sway_disagree = true;
                        }
                    } else {
                        seen_sway_socket = Some(value.to_owned());
                    }
                }
            }
            if fs::read_to_string(directory.join("comm")).is_ok_and(|comm| comm.trim() == "sway") {
                // Sway sets SWAYSOCK after exec, so /proc/<pid>/environ may
                // never expose it. Its socket name includes this proven
                // in-session compositor's uid and pid; the client also proves
                // socket ownership and peer credentials before using it.
                compositor_sockets.push(format!(
                    "{runtime_dir}/sway-ipc.{}.{}.sock",
                    self.uid,
                    name.to_string_lossy()
                ));
                if seen_desktop.is_none() {
                    seen_desktop = Some("sway".into());
                }
            }
        }

        if seen_sway_socket.is_none() && compositor_sockets.len() == 1 {
            seen_sway_socket = compositor_sockets.pop();
        }

        SessionRouteEnv {
            desktop: if desktop_disagree { None } else { seen_desktop },
            sway_socket: if sway_disagree {
                None
            } else {
                seen_sway_socket
            },
        }
    }
}

/// Proves a credential with a real connection to the session's X server.
#[derive(Default)]
pub struct NativeXServer;

/// Runs `work` on its own thread and gives up after `limit`, so a hung X server (one
/// that accepts the socket and never answers) can't stall the caller. A thread that
/// times out is left to finish on its own.
pub fn within<T: Send + 'static>(
    limit: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("solstone-x-connect".into())
        .spawn(move || {
            let _ = sender.send(work());
        })
        .ok()?;
    receiver.recv_timeout(limit).ok()
}

pub const X_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

impl XServer for NativeXServer {
    fn connect(&mut self, display: &str, credential: &XCredential) -> Result<(), String> {
        let display = display.to_owned();
        let credential = credential.clone();
        within(X_CONNECT_TIMEOUT, move || {
            connect_once(&display, &credential)
        })
        .unwrap_or_else(|| Err(format!("no answer after {}s", X_CONNECT_TIMEOUT.as_secs())))
    }
}

fn connect_once(display: &str, credential: &XCredential) -> Result<(), String> {
    use x11rb::{
        reexports::x11rb_protocol::parse_display::parse_display,
        rust_connection::{DefaultStream, RustConnection},
    };
    let parsed = parse_display(Some(display)).map_err(|error| error.to_string())?;
    let mut last_error = format!("no address for {display}");
    for address in parsed.connect_instruction() {
        match DefaultStream::connect(&address) {
            Ok((stream, _)) => {
                return RustConnection::connect_to_stream_with_auth_info(
                    stream,
                    usize::from(parsed.screen),
                    credential.name.clone(),
                    credential.data.clone(),
                )
                .map(drop)
                .map_err(|error| error.to_string());
            }
            Err(error) => last_error = error.to_string(),
        }
    }
    Err(last_error)
}

/// A resolver for `doctor`: the app's own view, read from this terminal's process,
/// that never rewrites the credential the running app keeps.
pub fn doctor_resolver() -> io::Result<NativeResolver> {
    let uid = rustix::process::getuid().as_raw();
    let mut resolver = Resolver::new(
        NativeLogind::new()?,
        ProcSessionProcesses::new(uid),
        NativeXServer,
        uid,
        managed_xauthority_path(std::env::var("XDG_RUNTIME_DIR").ok().as_deref(), uid),
        StartupEnvironment {
            wayland_display: std::env::var("WAYLAND_DISPLAY").ok(),
            display: std::env::var("DISPLAY").ok(),
            xauthority: std::env::var("XAUTHORITY").ok(),
            home: std::env::var_os("HOME").map(PathBuf::from),
        },
    );
    resolver.read_only = true;
    Ok(resolver)
}

/// Whether logind answers at all, so capture can wait for a session instead of exiting.
pub fn logind_answers(uid: u32) -> bool {
    NativeLogind::new().is_ok_and(|mut logind| logind.list(uid).is_ok())
}

pub type NativeResolver = Resolver<NativeLogind, ProcSessionProcesses, NativeXServer>;

/// The resolver plus the target it publishes for the video backend.
pub struct SessionFollower {
    resolver: NativeResolver,
    shared: SharedTarget,
    environment_read_at: Option<Instant>,
}

/// While no desktop is live, how often the user manager's environment is read again.
const SESSION_ENVIRONMENT_RECHECK: Duration = Duration::from_secs(60);

impl SessionFollower {
    pub fn new(resolver: NativeResolver) -> Self {
        let shared = Arc::new(Mutex::new(resolver.current().clone()));
        Self {
            resolver,
            shared,
            environment_read_at: None,
        }
    }

    pub fn shared(&self) -> SharedTarget {
        Arc::clone(&self.shared)
    }

    pub fn resolve(&mut self) -> CaptureTarget {
        let now = Instant::now();
        let was_live = self.resolver.current().presence.is_live();
        if !was_live
            && self
                .environment_read_at
                .is_none_or(|at| now.saturating_duration_since(at) >= SESSION_ENVIRONMENT_RECHECK)
        {
            self.environment_read_at = Some(now);
            reread_manager_environment();
            self.resolver.adopt_startup(startup_environment());
        }
        let target = self.resolver.resolve(now);
        if !was_live && self.resolver.current().presence.is_live() {
            reread_manager_environment();
            self.resolver.adopt_startup(startup_environment());
        }
        *self.shared.lock().expect("capture target lock") = target.clone();
        target
    }

    pub fn route_environment(&mut self, session_id: &str) -> SessionRouteEnv {
        self.resolver.route_environment(session_id)
    }
}

/// The production resolver, reading the startup environment kept by `cmd_run`.
pub fn native_resolver() -> io::Result<NativeResolver> {
    let uid = rustix::process::getuid().as_raw();
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    Ok(Resolver::new(
        NativeLogind::new()?,
        ProcSessionProcesses::new(uid),
        NativeXServer,
        uid,
        managed_xauthority_path(runtime_dir.as_deref(), uid),
        startup_environment(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    fn session(id: &str, kind: &str, active: bool) -> LoginSession {
        LoginSession {
            id: id.into(),
            path: format!("/org/freedesktop/login1/session/_3{id}"),
            uid: 1000,
            kind: kind.into(),
            class: "user".into(),
            active,
            remote: false,
            seat: "seat0".into(),
            display: if kind == "x11" {
                format!(":{id}")
            } else {
                String::new()
            },
            state: if active { "active" } else { "online" }.into(),
            desktop: String::new(),
        }
    }

    #[test]
    fn selects_the_active_local_desktop_of_this_user() {
        let sessions = vec![session("1", "x11", false), session("2", "x11", true)];
        assert_eq!(select(&sessions, 1000).unwrap().id, "2");
    }

    #[test]
    fn another_session_in_front_of_the_seat_is_not_active() {
        let mut other = session("3", "wayland", true);
        other.uid = 1001;
        let sessions = vec![session("1", "x11", false), other];
        assert_eq!(select(&sessions, 1000), Err(SessionPresence::NotActive));
    }

    #[test]
    fn the_greeter_and_text_sessions_are_never_captured() {
        let mut greeter = session("c1", "x11", true);
        greeter.class = "greeter".into();
        let tty = session("4", "tty", true);
        assert_eq!(
            select(&[greeter, tty], 1000),
            Err(SessionPresence::NoDesktop)
        );
    }

    #[test]
    fn remote_and_seatless_sessions_are_never_captured() {
        let mut remote = session("5", "x11", true);
        remote.remote = true;
        let mut seatless = session("6", "wayland", true);
        seatless.seat = String::new();
        assert_eq!(
            select(&[remote, seatless], 1000),
            Err(SessionPresence::NoDesktop)
        );
    }

    #[test]
    fn a_closing_session_is_neither_live_nor_waiting() {
        let mut closing = session("7", "x11", true);
        closing.state = "closing".into();
        closing.active = false;
        assert_eq!(select(&[closing], 1000), Err(SessionPresence::NoDesktop));
    }

    #[test]
    fn xauthority_round_trips_and_stops_at_truncation() {
        let entries = vec![
            XauthEntry {
                family: 256,
                address: b"host".to_vec(),
                number: b"0".to_vec(),
                name: b"MIT-MAGIC-COOKIE-1".to_vec(),
                data: vec![1; 16],
            },
            XauthEntry {
                family: FAMILY_WILD,
                address: vec![],
                number: b"1".to_vec(),
                name: b"MIT-MAGIC-COOKIE-1".to_vec(),
                data: vec![2; 16],
            },
        ];
        let bytes = encode_xauthority(&entries);
        assert_eq!(parse_xauthority(&bytes), entries);
        assert_eq!(parse_xauthority(&bytes[..bytes.len() - 3]), entries[..1]);
        assert_eq!(credentials_for_display(&bytes, 1).len(), 1);
        assert_eq!(credentials_for_display(&bytes, 1)[0].data, vec![2; 16]);
    }

    #[test]
    fn display_numbers_parse_like_xlib() {
        assert_eq!(display_number(":0"), Some(0));
        assert_eq!(display_number(":1.0"), Some(1));
    }

    type Script = Rc<RefCell<VecDeque<Result<Vec<LoginSession>, String>>>>;
    type Environments = Rc<RefCell<HashMap<String, Vec<HashMap<String, String>>>>>;

    struct Logind(Script);
    impl LoginSessions for Logind {
        fn list(&mut self, _: u32) -> Result<Vec<LoginSession>, String> {
            self.0
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Err("script ended".into()))
        }
    }

    struct Processes(Environments);
    impl SessionProcesses for Processes {
        fn environments(&mut self, id: &str) -> Vec<HashMap<String, String>> {
            self.0.borrow().get(id).cloned().unwrap_or_default()
        }
    }

    /// An X server per display that admits exactly one cookie.
    struct Servers(Rc<RefCell<HashMap<String, Vec<u8>>>>, Rc<RefCell<usize>>);
    impl XServer for Servers {
        fn connect(&mut self, display: &str, credential: &XCredential) -> Result<(), String> {
            *self.1.borrow_mut() += 1;
            match self.0.borrow().get(display) {
                None => Err("connection refused".into()),
                Some(cookie) if *cookie == credential.data => Ok(()),
                Some(_) => Err("Authorization required".into()),
            }
        }
    }

    struct Bed {
        temp: tempfile::TempDir,
        script: Script,
        processes: Environments,
        servers: Rc<RefCell<HashMap<String, Vec<u8>>>>,
        probes: Rc<RefCell<usize>>,
        resolver: Resolver<Logind, Processes, Servers>,
    }

    fn bed(startup_xauthority: Option<&str>) -> Bed {
        let temp = tempfile::tempdir().unwrap();
        let script = Rc::new(RefCell::new(VecDeque::new()));
        let processes = Rc::new(RefCell::new(HashMap::new()));
        let servers = Rc::new(RefCell::new(HashMap::new()));
        let probes = Rc::new(RefCell::new(0));
        let resolver = Resolver::new(
            Logind(Rc::clone(&script)),
            Processes(Rc::clone(&processes)),
            Servers(Rc::clone(&servers), Rc::clone(&probes)),
            1000,
            temp.path().join("run/solstone-linux/Xauthority"),
            StartupEnvironment {
                wayland_display: None,
                display: Some(":0".into()),
                xauthority: startup_xauthority.map(str::to_owned),
                home: Some(temp.path().join("home")),
            },
        );
        Bed {
            temp,
            script,
            processes,
            servers,
            probes,
            resolver,
        }
    }

    #[test]
    fn route_and_eligibility_share_one_selection_including_app_slice_desktops() {
        for next in [
            vec![LoginSession {
                desktop: "plasma".into(),
                ..session("2", "wayland", true)
            }],
            vec![],
        ] {
            let mut bed = bed(None);
            let first = LoginSession {
                desktop: "GNOME".into(),
                ..session("1", "wayland", true)
            };
            bed.script.borrow_mut().extend([Ok(vec![first]), Ok(next)]);
            let now = Instant::now();
            let target = bed.resolver.resolve(now);
            assert_eq!(target.desktop, "GNOME");
            assert_eq!(
                bed.resolver.route_environment("1").desktop.as_deref(),
                Some("GNOME")
            );
            assert_eq!(
                bed.script.borrow().len(),
                1,
                "route lookup must not select a second session"
            );
            let next = bed.resolver.resolve(now + Duration::from_secs(1));
            assert!(next.epoch > target.epoch);
            assert_eq!(
                bed.resolver.route_environment("1"),
                SessionRouteEnv::default()
            );
            if next.presence.is_live() {
                assert_eq!(
                    bed.resolver.route_environment("2").desktop.as_deref(),
                    Some("KDE")
                );
            }
        }
    }

    #[test]
    fn sway_socket_can_come_from_the_in_scope_compositor_without_startup_socket() {
        let root = tempfile::tempdir().unwrap();
        let uid = rustix::process::getuid().as_raw();
        for (pid, scope, env, comm) in [
            (
                "21",
                "session-3.scope",
                "XDG_CURRENT_DESKTOP=sway\0XDG_RUNTIME_DIR=/synthetic/runtime\0",
                "sway\n",
            ),
            (
                "22",
                "user@1000.service",
                "XDG_CURRENT_DESKTOP=GNOME\0SWAYSOCK=/wrong/startup.sock\0",
                "bash\n",
            ),
        ] {
            let dir = root.path().join(pid);
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("cgroup"), format!("0::/user.slice/{scope}\n")).unwrap();
            fs::write(dir.join("environ"), env).unwrap();
            fs::write(dir.join("comm"), comm).unwrap();
        }
        let mut processes = ProcSessionProcesses::with_root(root.path(), uid);
        let env = processes.route_environment("3");
        assert_eq!(env.desktop.as_deref(), Some("sway"));
        assert_eq!(
            env.sway_socket,
            Some(format!("/synthetic/runtime/sway-ipc.{uid}.21.sock"))
        );
    }

    fn cookie_file(bed: &Bed, name: &str, number: &str, cookie: &[u8]) -> String {
        let path = bed.temp.path().join(name);
        fs::write(
            &path,
            encode_xauthority(&[XauthEntry {
                family: 256,
                address: b"vm".to_vec(),
                number: number.as_bytes().to_vec(),
                name: b"MIT-MAGIC-COOKIE-1".to_vec(),
                data: cookie.to_vec(),
            }]),
        )
        .unwrap();
        path.to_string_lossy().into_owned()
    }

    fn session_env(bed: &Bed, id: &str, display: &str, xauthority: &str) {
        bed.processes.borrow_mut().insert(
            id.into(),
            vec![HashMap::from([
                ("DISPLAY".to_owned(), display.to_owned()),
                ("XAUTHORITY".to_owned(), xauthority.to_owned()),
            ])],
        );
    }

    #[test]
    fn a_second_login_moves_capture_to_its_display_and_credential() {
        let mut bed = bed(None);
        let first = cookie_file(&bed, "xauth_a", "1", b"cookie-a");
        let second = cookie_file(&bed, "xauth_b", "2", b"cookie-b");
        session_env(&bed, "1", ":1", &first);
        session_env(&bed, "2", ":2", &second);
        bed.servers
            .borrow_mut()
            .insert(":1".into(), b"cookie-a".to_vec());
        bed.servers
            .borrow_mut()
            .insert(":2".into(), b"cookie-b".to_vec());
        let now = Instant::now();
        bed.script
            .borrow_mut()
            .push_back(Ok(vec![session("1", "x11", true)]));
        let one = bed.resolver.resolve(now);
        assert_eq!(one.presence, SessionPresence::Live);
        assert_eq!(one.display.as_deref(), Some(":1"));
        bed.script.borrow_mut().push_back(Ok(vec![
            session("1", "x11", false),
            session("2", "x11", true),
        ]));
        let two = bed.resolver.resolve(now + Duration::from_secs(5));
        assert_eq!(two.session_id.as_deref(), Some("2"));
        assert_eq!(two.display.as_deref(), Some(":2"));
        assert_eq!(two.local_desktops, 2);
        assert_ne!(one.epoch, two.epoch);
        let managed = fs::read(bed.temp.path().join("run/solstone-linux/Xauthority")).unwrap();
        assert_eq!(credentials_for_display(&managed, 2)[0].data, b"cookie-b");
        let mode = fs::metadata(bed.temp.path().join("run/solstone-linux/Xauthority"))
            .unwrap()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_stale_imported_environment_never_names_the_session() {
        // The old autostart line imported the first login's display and credential.
        let mut bed = bed(None);
        let first = cookie_file(&bed, "xauth_a", "1", b"cookie-a");
        let second = cookie_file(&bed, "xauth_b", "2", b"cookie-b");
        bed.resolver.fallback_display = Some(":1".into());
        bed.resolver.fallback_files = vec![PathBuf::from(&first)];
        session_env(&bed, "2", ":2", &second);
        bed.servers
            .borrow_mut()
            .insert(":2".into(), b"cookie-b".to_vec());
        bed.script.borrow_mut().push_back(Ok(vec![
            session("1", "x11", false),
            session("2", "x11", true),
        ]));
        let target = bed.resolver.resolve(Instant::now());
        assert_eq!(target.presence, SessionPresence::Live);
        assert_eq!(target.display.as_deref(), Some(":2"));
    }

    #[test]
    fn a_credential_deleted_under_a_live_session_is_kept() {
        let mut bed = bed(None);
        let file = cookie_file(&bed, "xauth_a", "1", b"cookie-a");
        session_env(&bed, "1", ":1", &file);
        bed.servers
            .borrow_mut()
            .insert(":1".into(), b"cookie-a".to_vec());
        let now = Instant::now();
        bed.script
            .borrow_mut()
            .push_back(Ok(vec![session("1", "x11", true)]));
        assert!(bed.resolver.resolve(now).presence.is_live());
        fs::remove_file(&file).unwrap();
        // A fresh resolver (a restart) finds the private copy.
        let managed = bed.resolver.managed.clone();
        bed.resolver.proven = None;
        bed.script
            .borrow_mut()
            .push_back(Ok(vec![session("1", "x11", true)]));
        let after = bed.resolver.resolve(now + RECHECK_INTERVAL * 2);
        assert!(after.presence.is_live(), "{after:?}");
        assert!(managed.exists());
    }

    #[test]
    fn no_working_credential_is_unreachable_not_live() {
        let mut bed = bed(None);
        session_env(&bed, "1", ":1", "/nonexistent/xauth_gone");
        bed.servers
            .borrow_mut()
            .insert(":1".into(), b"cookie-a".to_vec());
        bed.script
            .borrow_mut()
            .push_back(Ok(vec![session("1", "x11", true)]));
        let target = bed.resolver.resolve(Instant::now());
        assert_eq!(target.presence, SessionPresence::Unreachable);
        assert!(
            target.detail.contains("Authorization required"),
            "{}",
            target.detail
        );
    }

    #[test]
    fn a_proven_credential_is_not_probed_again_until_the_recheck() {
        let mut bed = bed(None);
        let file = cookie_file(&bed, "xauth_a", "1", b"cookie-a");
        session_env(&bed, "1", ":1", &file);
        bed.servers
            .borrow_mut()
            .insert(":1".into(), b"cookie-a".to_vec());
        let now = Instant::now();
        for _ in 0..3 {
            bed.script
                .borrow_mut()
                .push_back(Ok(vec![session("1", "x11", true)]));
        }
        bed.resolver.resolve(now);
        bed.resolver.resolve(now + Duration::from_secs(5));
        assert_eq!(*bed.probes.borrow(), 1);
        bed.resolver.resolve(now + RECHECK_INTERVAL);
        assert_eq!(*bed.probes.borrow(), 2);
    }

    #[test]
    fn switching_away_and_back_waits_then_resumes() {
        let mut bed = bed(None);
        let file = cookie_file(&bed, "xauth_a", "1", b"cookie-a");
        session_env(&bed, "1", ":1", &file);
        bed.servers
            .borrow_mut()
            .insert(":1".into(), b"cookie-a".to_vec());
        let now = Instant::now();
        let mut bob = session("9", "wayland", true);
        bob.uid = 1001;
        bed.script.borrow_mut().extend([
            Ok(vec![session("1", "x11", true)]),
            Ok(vec![session("1", "x11", false), bob]),
            Ok(vec![session("1", "x11", true)]),
        ]);
        let live = bed.resolver.resolve(now);
        let away = bed.resolver.resolve(now + Duration::from_secs(5));
        let back = bed.resolver.resolve(now + Duration::from_secs(10));
        assert!(live.presence.is_live());
        assert_eq!(away.presence, SessionPresence::NotActive);
        assert!(back.presence.is_live());
        assert!(live.epoch < away.epoch && away.epoch < back.epoch);
    }

    #[test]
    fn a_logind_hiccup_keeps_the_answer_only_within_grace() {
        let mut bed = bed(None);
        bed.script
            .borrow_mut()
            .push_back(Ok(vec![session("1", "wayland", true)]));
        let now = Instant::now();
        let live = bed.resolver.resolve(now);
        assert!(live.presence.is_live());
        bed.script.borrow_mut().push_back(Err("timed out".into()));
        assert_eq!(bed.resolver.resolve(now + Duration::from_secs(5)), live);
        bed.script.borrow_mut().push_back(Err("timed out".into()));
        let lost = bed
            .resolver
            .resolve(now + LOGIND_GRACE + Duration::from_secs(5));
        assert_eq!(lost.presence, SessionPresence::LoginUnavailable);
    }

    #[test]
    fn wayland_is_live_without_any_x_credential() {
        let mut bed = bed(None);
        bed.script
            .borrow_mut()
            .push_back(Ok(vec![session("3", "wayland", true)]));
        let target = bed.resolver.resolve(Instant::now());
        assert!(target.presence.is_live());
        assert_eq!(target.kind, Some(DesktopKind::Wayland));
    }

    #[test]
    fn a_compositor_launched_from_a_text_login_is_a_desktop() {
        let mut bed = bed(None);
        bed.processes.borrow_mut().insert(
            "4".into(),
            vec![HashMap::from([(
                "WAYLAND_DISPLAY".to_owned(),
                "wayland-1".to_owned(),
            )])],
        );
        let mut sway = session("4", "tty", true);
        sway.display = String::new();
        let mut other = session("5", "tty", true);
        other.uid = 1001;
        bed.script.borrow_mut().push_back(Ok(vec![sway.clone()]));
        let target = bed.resolver.resolve(Instant::now());
        assert!(target.presence.is_live(), "{target:?}");
        assert_eq!(target.kind, Some(DesktopKind::Wayland));
        // A plain text login, or someone else's, is still never a desktop.
        bed.processes.borrow_mut().clear();
        bed.resolver.text_launched.clear();
        bed.script.borrow_mut().push_back(Ok(vec![sway, other]));
        assert_eq!(
            bed.resolver.resolve(Instant::now()).presence,
            SessionPresence::NoDesktop
        );
    }

    #[test]
    fn a_closing_session_does_not_count_as_a_second_desktop() {
        let mut bed = bed(None);
        let mut closing = session("1", "wayland", false);
        closing.state = "closing".into();
        bed.script
            .borrow_mut()
            .push_back(Ok(vec![closing, session("2", "wayland", true)]));
        assert_eq!(bed.resolver.resolve(Instant::now()).local_desktops, 1);
    }

    #[test]
    fn the_kept_credential_keeps_other_displays_entries() {
        let temp = tempfile::tempdir().unwrap();
        let managed = temp.path().join("run/solstone-linux/Xauthority");
        let seed = temp.path().join("startup");
        let entry = |number: &str, data: &[u8]| XauthEntry {
            family: 256,
            address: b"vm".to_vec(),
            number: number.as_bytes().to_vec(),
            name: b"MIT-MAGIC-COOKIE-1".to_vec(),
            data: data.to_vec(),
        };
        fs::write(
            &seed,
            encode_xauthority(&[entry("0", b"zero"), entry("1", b"old")]),
        )
        .unwrap();
        let cookie = XCredential {
            name: b"MIT-MAGIC-COOKIE-1".to_vec(),
            data: b"new".to_vec(),
        };
        write_managed(&managed, Some(&seed), 1, &cookie).unwrap();
        let kept = fs::read(&managed).unwrap();
        assert_eq!(credentials_for_display(&kept, 1)[0].data, b"new");
        assert_eq!(credentials_for_display(&kept, 1).len(), 1);
        assert_eq!(credentials_for_display(&kept, 0)[0].data, b"zero");
    }

    #[test]
    fn a_hung_connect_gives_up() {
        let started = Instant::now();
        let result = within(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_secs(2));
        });
        assert!(result.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn proc_scan_reads_only_this_users_processes_in_the_scope() {
        let temp = tempfile::tempdir().unwrap();
        let uid = rustix::process::getuid().as_raw();
        for (pid, cgroup, environ) in [
            (
                "10",
                "0::/user.slice/user-1000.slice/session-2.scope\n",
                "DISPLAY=:2\0XAUTHORITY=/tmp/xauth_b\0HOME=/home/a\0",
            ),
            (
                "11",
                "0::/user.slice/user-1000.slice/session-21.scope\n",
                "DISPLAY=:9\0",
            ),
            (
                "12",
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice\n",
                "DISPLAY=:1\0",
            ),
        ] {
            let directory = temp.path().join(pid);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("cgroup"), cgroup).unwrap();
            fs::write(directory.join("environ"), environ).unwrap();
        }
        fs::create_dir(temp.path().join("self")).unwrap();
        let mut scan = ProcSessionProcesses::with_root(temp.path(), uid);
        let found = scan.environments("2");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["DISPLAY"], ":2");
        assert_eq!(found[0]["XAUTHORITY"], "/tmp/xauth_b");
        assert!(!found[0].contains_key("HOME"));
        assert!(
            ProcSessionProcesses::with_root(temp.path(), uid + 1)
                .environments("2")
                .is_empty()
        );
    }

    #[test]
    fn test_proc_session_route_environment() {
        let temp = tempfile::tempdir().unwrap();
        let uid = rustix::process::getuid().as_raw();
        for (pid, cgroup, environ) in [
            (
                "21",
                "0::/user.slice/user-1000.slice/session-3.scope\n",
                "XDG_CURRENT_DESKTOP=sway\0SWAYSOCK=/run/user/1000/sway.sock\0",
            ),
            (
                "22",
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice\n",
                "XDG_CURRENT_DESKTOP=GNOME\0SWAYSOCK=/run/user/1000/other.sock\0",
            ),
        ] {
            let directory = temp.path().join(pid);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("cgroup"), cgroup).unwrap();
            fs::write(directory.join("environ"), environ).unwrap();
        }

        let mut scan = ProcSessionProcesses::with_root(temp.path(), uid);
        let route_env = scan.route_environment("3");
        assert_eq!(route_env.desktop.as_deref(), Some("sway"));
        assert_eq!(
            route_env.sway_socket.as_deref(),
            Some("/run/user/1000/sway.sock")
        );

        // Add a disagreeing process in scope for desktop only
        let dir23 = temp.path().join("23");
        fs::create_dir(&dir23).unwrap();
        fs::write(
            dir23.join("cgroup"),
            "0::/user.slice/user-1000.slice/session-3.scope\n",
        )
        .unwrap();
        fs::write(dir23.join("environ"), "XDG_CURRENT_DESKTOP=GNOME\0").unwrap();

        let route_env2 = scan.route_environment("3");
        assert_eq!(route_env2.desktop, None);
        assert_eq!(
            route_env2.sway_socket.as_deref(),
            Some("/run/user/1000/sway.sock")
        );
    }
}
