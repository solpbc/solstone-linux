// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Panel-icon readiness and the one consented offer to restore it.
//!
//! The tray speaks StatusNotifierItem over D-Bus, which renders nothing unless some
//! process owns `org.kde.StatusNotifierWatcher`. KDE ships a watcher; stock GNOME does
//! not, so a GNOME owner gets no panel icon and therefore no desktop pause control.
//!
//! Everything here reads the watcher's *bus-name ownership*, live, at the moment a
//! decision depends on it. `ksni::OfflineReason` is deliberately not that signal: its
//! `No` variant covers both a transient shell restart and a permanently disabled
//! extension, so it cannot distinguish "wait" from "offer to fix". The settle window in
//! [`OfferGate`] is what separates them.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use zbus::{Connection, zvariant::OwnedValue};

/// Upstream's uuid. This is what extensions.gnome.org serves and what Fedora's
/// `gnome-shell-extension-appindicator` RPM installs.
pub const APPINDICATOR_UUID: &str = "appindicatorsupport@rgcjonas.gmail.com";
/// The uuid Debian *and* Ubuntu both ship under the same package name. Measured from
/// the package itself, not assumed: a deb-family owner who already has the extension has
/// it under this uuid, so reading only the upstream one reports it absent and offers to
/// install a second per-user copy that would race the system one for the watcher name.
pub const UBUNTU_APPINDICATOR_UUID: &str = "ubuntu-appindicators@ubuntu.com";
const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const SHELL_NAME: &str = "org.gnome.Shell";
const SHELL_PATH: &str = "/org/gnome/Shell";
const EXTENSIONS_INTERFACE: &str = "org.gnome.Shell.Extensions";
/// GNOME's `ExtensionState.ACTIVE` (named `ENABLED` before GNOME 46; the *value* is 1 in
/// both). Every other value means present but not loaded, including `ERROR`,
/// `OUT_OF_DATE` and the `INITIALIZED` a fresh package install leaves behind.
const EXTENSION_STATE_ACTIVE: f64 = 1.0;
/// How long to wait for GNOME to actually load an extension after it says it enabled one.
const ACTIVATION_GRACE: Duration = Duration::from_secs(5);

/// How long the panel icon must stay unavailable before the offer is allowed to appear.
///
/// A GNOME Shell restart drops the watcher for a few seconds. Offering inside that
/// window would fire the notification on every restart, which is the nag this is
/// deliberately not.
pub const OFFER_SETTLE_SECONDS: f64 = 90.0;
/// How often the running app re-reads watcher ownership. Short enough that enabling the
/// extension mid-session brings the icon back without restarting the app.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(20);
/// Bound on any single probe, so a wedged session bus cannot stall the caller.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What GNOME Shell says about the AppIndicator extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionState {
    /// Loaded and running. This is the only state that can produce a watcher.
    Active,
    /// Installed but not loaded — disabled, errored, out of date, or merely initialized.
    /// The trace that produced this work found `INITIALIZED` after a package install,
    /// which the old check reported as success.
    Present,
    /// On disk, but GNOME Shell has not picked it up. This is what a distribution's
    /// package leaves behind until the session restarts, and it is ⛔ NOT a state to
    /// offer an install for: the files are already there, and installing again would
    /// drop a second per-user copy beside the system one.
    OnDiskUnseen,
    /// GNOME Shell knows nothing about it, and neither does the filesystem.
    Absent,
    /// There is no GNOME Shell to ask.
    Unknown,
}

/// The answer `doctor` reports and the offer keys on. The middle three are the split
/// that replaced a single check whose only answers were "present" and "install it".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelIconReadiness {
    /// Something owns the watcher name, so the panel icon can appear.
    Available,
    /// No watcher and no GNOME Shell. Nothing here can fix it, and nothing is wrong.
    NotApplicable,
    /// We could not find out. ⛔ Deliberately its own answer rather than folded into
    /// `NotApplicable`: a session bus that will not answer is indistinguishable from a
    /// non-GNOME desktop at the call site, and reporting it as one turns a broken probe
    /// into a green line.
    Unknown,
    /// The extension is loaded but no watcher yet. Almost always a shell still starting.
    ExtensionActiveNoHost,
    /// Installed and switched off. The old check called this `ok`.
    ExtensionOff,
    /// On disk but not yet known to the running shell. Only a session restart fixes it.
    ExtensionNeedsRelogin,
    /// Not installed at all.
    ExtensionMissing,
}

impl PanelIconReadiness {
    /// Whether the offer has something it could actually do about this state.
    pub fn is_fixable(self) -> bool {
        matches!(self, Self::ExtensionOff | Self::ExtensionMissing)
    }
}

/// Fold three independent live facts into the reported answer.
///
/// Watcher ownership wins over everything: a desktop with a working panel icon is
/// `Available` whether or not it got there through the extension we know about.
pub fn readiness(
    watcher_present: bool,
    gnome_shell_present: bool,
    extension: ExtensionState,
) -> PanelIconReadiness {
    if watcher_present {
        return PanelIconReadiness::Available;
    }
    if !gnome_shell_present {
        return PanelIconReadiness::NotApplicable;
    }
    match extension {
        ExtensionState::Active => PanelIconReadiness::ExtensionActiveNoHost,
        ExtensionState::Present => PanelIconReadiness::ExtensionOff,
        ExtensionState::OnDiskUnseen => PanelIconReadiness::ExtensionNeedsRelogin,
        ExtensionState::Absent | ExtensionState::Unknown => PanelIconReadiness::ExtensionMissing,
    }
}

/// Every directory GNOME Shell loads extensions from.
///
/// Read directly because the shell only rescans these at startup: between a package
/// landing and the next login, the files exist and `GetExtensionInfo` still answers
/// "unknown". Asking only the shell there produces "not installed", and the remedy for
/// "not installed" is an install that is not what this owner needs.
fn extension_directories() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(home));
    } else if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share"));
    }
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let system: Vec<&str> = system.split(':').filter(|part| !part.is_empty()).collect();
    if system.is_empty() {
        roots.push(PathBuf::from("/usr/local/share"));
        roots.push(PathBuf::from("/usr/share"));
    } else {
        roots.extend(system.into_iter().map(PathBuf::from));
    }
    roots
        .into_iter()
        .map(|root| root.join("gnome-shell/extensions"))
        .collect()
}

/// Is any AppIndicator extension unpacked on this machine?
pub fn extension_on_disk() -> bool {
    extension_directories().into_iter().any(|directory| {
        [APPINDICATOR_UUID, UBUNTU_APPINDICATOR_UUID]
            .into_iter()
            .any(|uuid| directory.join(uuid).join("metadata.json").is_file())
    })
}

/// At most one offer per run, and only after the condition has held for the settle window.
///
/// Recovery resets the clock but not `offered`: an owner who enables the extension and
/// later switches it off again in the same session chose that, and does not get asked
/// about it a second time.
#[derive(Debug, Default)]
pub struct OfferGate {
    unavailable_since: Option<f64>,
    offered: bool,
}

impl OfferGate {
    /// Feed one probe result. `now` is monotonic seconds. Returns true exactly once.
    pub fn observe(&mut self, readiness: PanelIconReadiness, enabled: bool, now: f64) -> bool {
        if !readiness.is_fixable() {
            self.unavailable_since = None;
            return false;
        }
        let since = *self.unavailable_since.get_or_insert(now);
        if self.offered || !enabled || now - since < OFFER_SETTLE_SECONDS {
            return false;
        }
        self.offered = true;
        true
    }

    /// Whether this run has already shown its offer.
    pub fn offered(&self) -> bool {
        self.offered
    }
}

// Owner-visible strings. Gathered here rather than scattered across the call sites so a
// voice pass can read the whole surface at once.

/// The notification summary, matching the app's existing notification.
pub const OFFER_SUMMARY: &str = "solstone app";
pub const OFFER_BODY: &str = "GNOME needs one extension before the panel icon can appear. that icon is where pause and resume live.";
pub const OFFER_ACTION_SET_UP: &str = "set-up";
pub const OFFER_LABEL_SET_UP: &str = "set it up";
pub const OFFER_ACTION_LATER: &str = "later";
pub const OFFER_LABEL_LATER: &str = "not now";
pub const OFFER_ACTION_DISMISS: &str = "dismiss";
pub const OFFER_LABEL_DISMISS: &str = "don't ask again";

/// What the owner is told after accepting the offer, or nothing at all.
///
/// A cancel gets no follow-up notification: the owner has just declined a dialog, and
/// telling them so is the nag this design is avoiding.
pub fn offer_result_body(outcome: &SetupOutcome) -> Option<String> {
    match outcome {
        SetupOutcome::Installed => {
            Some("the panel icon is ready. pause and resume are in the panel now.".into())
        }
        SetupOutcome::AcceptedNotYetRunning => Some(
            "GNOME took the extension but the panel icon has not appeared. run solstone-linux doctor to see where it got to."
                .into(),
        ),
        // ⛔ The reason is carried, never dropped for a "run it again to find out": we
        // already know why, and re-running would only raise GNOME's dialog a second time.
        SetupOutcome::Failed(reason) => Some(format!("the panel icon is not set up: {reason}")),
        SetupOutcome::NeedsRelogin => Some(
            "the panel icon extension is installed. log out and back in, then run solstone-linux panel-icon again."
                .into(),
        ),
        SetupOutcome::Cancelled | SetupOutcome::AlreadyActive | SetupOutcome::NothingToSetUp => {
            None
        }
    }
}

/// What actually happened when the owner accepted the offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupOutcome {
    /// Verified: the extension is loaded and the panel icon can appear.
    Installed,
    /// GNOME accepted it but the panel icon has not come up yet. An honest middle
    /// answer, and ⛔ never folded into `Installed`: GNOME reporting success is a
    /// different fact from an extension running.
    AcceptedNotYetRunning,
    /// The owner declined GNOME's own confirmation. Not a failure.
    Cancelled,
    /// The files are on disk and only a session restart will load them. Nothing to
    /// install, nothing to turn on.
    NeedsRelogin,
    /// This desktop has no panel icon and nothing here can give it one. Not an error.
    NothingToSetUp,
    /// It was already running; nothing to do.
    AlreadyActive,
    /// GNOME refused, was unreachable, or could not fetch the extension.
    Failed(String),
}

/// Map `InstallRemoteExtension`'s reply string onto an outcome.
///
/// GNOME returns exactly `"successful"` or `"cancelled"`; every other outcome arrives as
/// a D-Bus *error*, not a string. An unrecognised string is reported rather than guessed
/// at, because a silently-mapped unknown reply is how a refusal reads as a success.
pub fn install_reply_outcome(reply: &str) -> SetupOutcome {
    match reply {
        "successful" => SetupOutcome::Installed,
        "cancelled" => SetupOutcome::Cancelled,
        other => SetupOutcome::Failed(format!("GNOME gave an answer we do not know: {other}")),
    }
}

/// Turn a D-Bus error name into something the owner can act on.
///
/// GNOME raises `org.gnome.Shell.Extensions.Error.*` for everything that is not a plain
/// yes or no, and the raw name means nothing to the person reading it.
pub fn install_error_reason(error_name: &str, fallback: &str) -> String {
    match error_name.rsplit('.').next().unwrap_or_default() {
        "NotAllowed" => "this desktop does not allow extensions to be installed".into(),
        "InfoDownloadFailed" | "DownloadFailed" => {
            "the extension could not be downloaded; check the network connection".into()
        }
        "ExtractFailed" => "the downloaded extension could not be unpacked".into(),
        "EnableFailed" => "GNOME installed the extension but could not turn it on".into(),
        _ => fallback.to_owned(),
    }
}

/// Read `state` out of an extension-info dictionary.
///
/// GNOME serializes this metadata through `GLib.Variant` and every JS number packs as a
/// **double**, so `state` arrives as `d` rather than `u` however integral it looks.
/// Accept the integer forms too rather than pinning one wire type.
///
/// ⛔ Deliberately not keyed on the sibling `enabled` boolean (GNOME 46+). That reports
/// the owner's *intent* -- whether the uuid sits in `enabled-extensions` -- and is `true`
/// for an extension that failed to load, or that is suppressed by
/// `disable-user-extensions`. Only `state` says whether anything is running, and only
/// something running can own the watcher name.
///
/// An unknown uuid is not an error on this interface: GNOME returns an empty dictionary.
pub fn extension_state_from_info(info: &HashMap<String, OwnedValue>) -> ExtensionState {
    if info.is_empty() {
        return ExtensionState::Absent;
    }
    let Some(state) = info.get("state") else {
        // In the dictionary but with no state we can read. It exists, and that is the
        // half this decision needs.
        return ExtensionState::Present;
    };
    let numeric = f64::try_from(state)
        .ok()
        .or_else(|| u32::try_from(state).ok().map(f64::from))
        .or_else(|| i64::try_from(state).ok().map(|value| value as f64));
    match numeric {
        Some(value) if (value - EXTENSION_STATE_ACTIVE).abs() < f64::EPSILON => {
            ExtensionState::Active
        }
        _ => ExtensionState::Present,
    }
}

/// What `solstone-linux panel-icon` prints before it touches anything.
pub fn command_preamble(readiness: PanelIconReadiness) -> &'static str {
    match readiness {
        PanelIconReadiness::Available => "the panel icon is already available on this desktop.",
        PanelIconReadiness::ExtensionActiveNoHost => {
            "the GNOME extension is already on. if the panel icon is still missing, log out and back in."
        }
        PanelIconReadiness::NotApplicable => {
            "nothing here can set up a panel icon on this desktop."
        }
        // The probe has already run by the time this prints, so it cannot say
        // "checking".
        PanelIconReadiness::Unknown => "could not reach this desktop to find out what it needs.",
        PanelIconReadiness::ExtensionOff => {
            "the GNOME extension is installed but turned off. turning it on now."
        }
        PanelIconReadiness::ExtensionNeedsRelogin => {
            "the panel icon extension is installed, but GNOME has not picked it up yet."
        }
        PanelIconReadiness::ExtensionMissing => {
            "GNOME will ask you to confirm. accept it to add the panel icon extension."
        }
    }
}

/// What `solstone-linux panel-icon` prints when it is done, and the exit code with it.
///
/// A cancel is not an error: the owner was asked and said no.
pub fn command_result(outcome: &SetupOutcome) -> (String, i32) {
    match outcome {
        SetupOutcome::Installed => ("done. the panel icon is in your panel now.".into(), 0),
        SetupOutcome::AlreadyActive => ("nothing to do.".into(), 0),
        // ⛔ Not "then the panel icon will be there": a package install lands the
        // extension INITIALIZED, so the state after a relogin is installed-but-off and
        // one more run of this command is what finishes it.
        SetupOutcome::NeedsRelogin => (
            "log out and back in, then run this again.".into(),
            0,
        ),
        SetupOutcome::NothingToSetUp => ("nothing to set up here.".into(), 0),
        SetupOutcome::Cancelled => (
            "no change. you can run this again any time.".into(),
            0,
        ),
        SetupOutcome::AcceptedNotYetRunning => (
            "GNOME took the extension but the panel icon has not appeared yet. give it a moment, then run: solstone-linux doctor"
                .into(),
            1,
        ),
        // ⛔ Not "GNOME did not add the extension": three of the reasons that reach
        // here are not an install and one of them is our own probe failing.
        SetupOutcome::Failed(reason) => (format!("the panel icon is not set up: {reason}"), 1),
    }
}

async fn name_has_owner(connection: &Connection, name: &str) -> Result<bool, String> {
    let proxy = zbus::fdo::DBusProxy::new(connection)
        .await
        .map_err(|error| error.to_string())?;
    let name = zbus::names::BusName::try_from(name).map_err(|error| error.to_string())?;
    proxy
        .name_has_owner(name)
        .await
        .map_err(|error| error.to_string())
}

/// Does anything own the StatusNotifier watcher right now?
pub async fn watcher_present(connection: &Connection) -> Result<bool, String> {
    name_has_owner(connection, WATCHER_NAME).await
}

/// Is GNOME Shell on this session bus?
///
/// Bus ownership rather than `XDG_CURRENT_DESKTOP`, because the environment variable
/// says what the session claims to be and this says what can actually answer.
pub async fn gnome_shell_present(connection: &Connection) -> Result<bool, String> {
    name_has_owner(connection, SHELL_NAME).await
}

async fn extensions_proxy(connection: &Connection) -> Result<zbus::Proxy<'_>, String> {
    zbus::Proxy::new(connection, SHELL_NAME, SHELL_PATH, EXTENSIONS_INTERFACE)
        .await
        .map_err(|error| error.to_string())
}

/// Ask GNOME Shell about one extension uuid.
async fn extension_state_for(
    connection: &Connection,
    uuid: &str,
) -> Result<ExtensionState, String> {
    let proxy = extensions_proxy(connection).await?;
    let info: HashMap<String, OwnedValue> = proxy
        .call("GetExtensionInfo", &(uuid))
        .await
        .map_err(|error| error.to_string())?;
    Ok(extension_state_from_info(&info))
}

/// The best state across the uuids that can produce a watcher.
///
/// Ubuntu ships Canonical's fork under its own uuid and Fedora ships upstream's, so an
/// owner can legitimately have either. Reporting the better of the two keeps us from
/// offering to install something that is already working under another name.
pub async fn extension_state(connection: &Connection) -> ExtensionState {
    let mut best = if extension_on_disk() {
        ExtensionState::OnDiskUnseen
    } else {
        ExtensionState::Absent
    };
    for uuid in [APPINDICATOR_UUID, UBUNTU_APPINDICATOR_UUID] {
        match extension_state_for(connection, uuid).await {
            Ok(ExtensionState::Active) => return ExtensionState::Active,
            Ok(ExtensionState::Present) => best = ExtensionState::Present,
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(%error, uuid, "Could not read GNOME extension state");
                if best == ExtensionState::Absent {
                    best = ExtensionState::Unknown;
                }
            }
        }
    }
    best
}

/// One bounded read of every fact the answer depends on.
pub async fn probe(connection: &Connection) -> PanelIconReadiness {
    let probe = async {
        // ⛔ Never `unwrap_or(false)` here. A bus that errors would then assert "nothing
        // owns the watcher" and "there is no GNOME Shell" — two positive claims out of
        // one failure to ask, and `NotApplicable` renders as a green line.
        let Ok(watcher) = watcher_present(connection).await else {
            return PanelIconReadiness::Unknown;
        };
        if watcher {
            return PanelIconReadiness::Available;
        }
        let Ok(shell) = gnome_shell_present(connection).await else {
            return PanelIconReadiness::Unknown;
        };
        if !shell {
            return PanelIconReadiness::NotApplicable;
        }
        readiness(false, true, extension_state(connection).await)
    };
    match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
        Ok(value) => value,
        Err(_) => {
            tracing::debug!("Panel icon probe timed out");
            PanelIconReadiness::Unknown
        }
    }
}

/// Is GNOME loading user extensions at all?
///
/// Backed by the inverse of the `disable-user-extensions` setting. When it is off an
/// extension can install, report itself enabled, and never load — so acting without
/// reading this first produces a confident success and no panel icon.
pub async fn user_extensions_enabled(connection: &Connection) -> Option<bool> {
    let proxy = extensions_proxy(connection).await.ok()?;
    proxy
        .get_property::<bool>("UserExtensionsEnabled")
        .await
        .ok()
}

/// Act on the owner's consent, then check what actually happened.
///
/// Two paths, because the two reachable states need different calls. A missing extension
/// goes through `InstallRemoteExtension`, which renders GNOME's own confirmation and then
/// downloads, loads and enables it live. An extension that is already on disk but
/// switched off — the exact state a distribution's weak dependency leaves behind — needs
/// `EnableExtension` instead; installing it again is not what it is missing.
///
/// 🔴 Neither call is trusted for the answer. `InstallRemoteExtension` returning
/// `"successful"` means the owner pressed Install; `EnableExtension` returning `true`
/// means a uuid reached a settings list. Activation happens afterwards, asynchronously,
/// and can still fail. The watcher coming up is the only fact that matters here, so that
/// is what gets reported.
pub async fn set_up(connection: &Connection, readiness: PanelIconReadiness) -> SetupOutcome {
    match readiness {
        PanelIconReadiness::Available | PanelIconReadiness::ExtensionActiveNoHost => {
            return SetupOutcome::AlreadyActive;
        }
        PanelIconReadiness::NotApplicable => return SetupOutcome::NothingToSetUp,
        PanelIconReadiness::Unknown => {
            return SetupOutcome::Failed(
                "could not reach the desktop to check what it needs".into(),
            );
        }
        PanelIconReadiness::ExtensionNeedsRelogin => return SetupOutcome::NeedsRelogin,
        _ => {}
    }
    if user_extensions_enabled(connection).await == Some(false) {
        return SetupOutcome::Failed(
            "GNOME is set not to load extensions, so the panel icon cannot come back this way"
                .into(),
        );
    }
    let attempted = if readiness == PanelIconReadiness::ExtensionOff {
        enable(connection).await
    } else {
        install(connection).await
    };
    match attempted {
        SetupOutcome::Installed => confirm_running(connection).await,
        other => other,
    }
}

/// Wait a bounded moment for the watcher to actually come up, and report what is true.
async fn confirm_running(connection: &Connection) -> SetupOutcome {
    let deadline = tokio::time::Instant::now() + ACTIVATION_GRACE;
    loop {
        if watcher_present(connection).await.unwrap_or(false) {
            return SetupOutcome::Installed;
        }
        if tokio::time::Instant::now() >= deadline {
            return SetupOutcome::AcceptedNotYetRunning;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

async fn install(connection: &Connection) -> SetupOutcome {
    let proxy = match extensions_proxy(connection).await {
        Ok(proxy) => proxy,
        Err(error) => return SetupOutcome::Failed(error),
    };
    // Deliberately unbounded: this call does not return until the owner answers GNOME's
    // confirmation dialog, and a timeout here would report a refusal the owner never made.
    match proxy
        .call::<_, _, String>("InstallRemoteExtension", &(APPINDICATOR_UUID))
        .await
    {
        Ok(reply) => install_reply_outcome(&reply),
        Err(error) => {
            let name = match &error {
                zbus::Error::MethodError(name, _, _) => name.as_str().to_owned(),
                _ => String::new(),
            };
            SetupOutcome::Failed(install_error_reason(&name, &error.to_string()))
        }
    }
}

async fn enable(connection: &Connection) -> SetupOutcome {
    let proxy = match extensions_proxy(connection).await {
        Ok(proxy) => proxy,
        Err(error) => return SetupOutcome::Failed(error),
    };
    for uuid in [APPINDICATOR_UUID, UBUNTU_APPINDICATOR_UUID] {
        if !matches!(
            extension_state_for(connection, uuid).await,
            Ok(ExtensionState::Present)
        ) {
            continue;
        }
        match proxy.call::<_, _, bool>("EnableExtension", &(uuid)).await {
            // `true` only means the uuid reached the settings list. set_up confirms it.
            Ok(true) => return SetupOutcome::Installed,
            Ok(false) => {
                tracing::warn!(uuid, "GNOME refused to enable the extension");
                return SetupOutcome::Failed("GNOME would not turn the extension on".into());
            }
            Err(error) => return SetupOutcome::Failed(error.to_string()),
        }
    }
    SetupOutcome::Failed("no installed appindicator extension to turn on".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_watcher_wins_over_every_extension_state() {
        for state in [
            ExtensionState::Active,
            ExtensionState::Present,
            ExtensionState::Absent,
            ExtensionState::Unknown,
        ] {
            assert_eq!(
                readiness(true, true, state),
                PanelIconReadiness::Available,
                "{state:?}"
            );
            assert_eq!(
                readiness(true, false, state),
                PanelIconReadiness::Available,
                "{state:?}"
            );
        }
    }

    #[test]
    fn no_watcher_and_no_gnome_shell_is_not_applicable() {
        assert_eq!(
            readiness(false, false, ExtensionState::Absent),
            PanelIconReadiness::NotApplicable
        );
    }

    #[test]
    fn an_installed_but_disabled_extension_is_never_available() {
        // The defect this work exists to fix: the old check reported this `ok`.
        let answer = readiness(false, true, ExtensionState::Present);
        assert_eq!(answer, PanelIconReadiness::ExtensionOff);
        assert_ne!(answer, PanelIconReadiness::Available);
        assert!(answer.is_fixable());
    }

    #[test]
    fn files_on_disk_the_shell_has_not_seen_are_never_offered_an_install() {
        // The state a distribution's weak dependency creates: the package landed, the
        // shell has not rescanned, and GetExtensionInfo still answers "unknown".
        // Installing here would drop a second per-user copy beside the system one.
        let answer = readiness(false, true, ExtensionState::OnDiskUnseen);
        assert_eq!(answer, PanelIconReadiness::ExtensionNeedsRelogin);
        assert!(!answer.is_fixable());
        assert_ne!(answer, PanelIconReadiness::ExtensionMissing);
    }

    #[test]
    fn the_relogin_state_tells_the_owner_rather_than_acting() {
        let (message, code) = command_result(&SetupOutcome::NeedsRelogin);
        assert!(message.contains("log out"));
        assert_eq!(code, 0, "a session restart is not an error");
        assert!(
            offer_result_body(&SetupOutcome::NeedsRelogin).is_some_and(|b| b.contains("log out"))
        );
    }

    #[test]
    fn the_three_gnome_answers_are_distinct() {
        let answers = [
            readiness(false, true, ExtensionState::Active),
            readiness(false, true, ExtensionState::Present),
            readiness(false, true, ExtensionState::Absent),
        ];
        assert_eq!(
            answers,
            [
                PanelIconReadiness::ExtensionActiveNoHost,
                PanelIconReadiness::ExtensionOff,
                PanelIconReadiness::ExtensionMissing
            ]
        );
    }

    #[test]
    fn an_active_extension_without_a_host_is_not_offered_a_fix() {
        // OfflineReason::No covers a shell restart too. Nothing to install here.
        assert!(!readiness(false, true, ExtensionState::Active).is_fixable());
    }

    #[test]
    fn the_offer_waits_out_the_settle_window() {
        let mut gate = OfferGate::default();
        assert!(!gate.observe(PanelIconReadiness::ExtensionMissing, true, 0.0));
        assert!(!gate.observe(
            PanelIconReadiness::ExtensionMissing,
            true,
            OFFER_SETTLE_SECONDS - 0.1
        ));
        assert!(gate.observe(
            PanelIconReadiness::ExtensionMissing,
            true,
            OFFER_SETTLE_SECONDS
        ));
    }

    #[test]
    fn a_shell_restart_inside_the_settle_window_never_offers() {
        let mut gate = OfferGate::default();
        // Watcher drops at t=0, comes back at t=10, drops again at t=20. Without the
        // reset the second gap would inherit the first one's clock and fire early.
        assert!(!gate.observe(PanelIconReadiness::ExtensionMissing, true, 0.0));
        assert!(!gate.observe(PanelIconReadiness::Available, true, 10.0));
        assert!(!gate.observe(PanelIconReadiness::ExtensionMissing, true, 20.0));
        assert!(!gate.observe(
            PanelIconReadiness::ExtensionMissing,
            true,
            20.0 + OFFER_SETTLE_SECONDS - 0.1
        ));
        assert!(gate.observe(
            PanelIconReadiness::ExtensionMissing,
            true,
            20.0 + OFFER_SETTLE_SECONDS
        ));
    }

    #[test]
    fn the_offer_fires_at_most_once_per_run() {
        let mut gate = OfferGate::default();
        // The clock starts at the first unavailable observation, not at zero.
        assert!(!gate.observe(PanelIconReadiness::ExtensionMissing, true, 0.0));
        assert!(gate.observe(
            PanelIconReadiness::ExtensionMissing,
            true,
            OFFER_SETTLE_SECONDS
        ));
        assert!(gate.offered());
        for step in 1..10 {
            let now = OFFER_SETTLE_SECONDS * f64::from(step + 1);
            assert!(!gate.observe(PanelIconReadiness::ExtensionMissing, true, now));
        }
    }

    #[test]
    fn recovering_and_failing_again_does_not_re_offer() {
        let mut gate = OfferGate::default();
        assert!(!gate.observe(PanelIconReadiness::ExtensionMissing, true, 0.0));
        assert!(gate.observe(
            PanelIconReadiness::ExtensionMissing,
            true,
            OFFER_SETTLE_SECONDS
        ));
        assert!(!gate.observe(PanelIconReadiness::Available, true, 200.0));
        assert!(!gate.observe(PanelIconReadiness::ExtensionMissing, true, 400.0));
    }

    #[test]
    fn a_dismissed_offer_never_fires() {
        let mut gate = OfferGate::default();
        for step in 0..20 {
            let now = f64::from(step) * OFFER_SETTLE_SECONDS;
            assert!(!gate.observe(PanelIconReadiness::ExtensionMissing, false, now));
        }
        assert!(!gate.offered());
        // ...and re-enabling it by hand still works, because nothing was consumed: the
        // clock has been running the whole time, so the very next probe offers.
        assert!(gate.observe(
            PanelIconReadiness::ExtensionMissing,
            true,
            20.0 * OFFER_SETTLE_SECONDS
        ));
    }

    #[test]
    fn install_replies_map_to_outcomes_and_unknowns_are_not_success() {
        assert_eq!(install_reply_outcome("successful"), SetupOutcome::Installed);
        assert_eq!(install_reply_outcome("cancelled"), SetupOutcome::Cancelled);
        assert!(matches!(
            install_reply_outcome("something-else"),
            SetupOutcome::Failed(_)
        ));
        assert!(matches!(install_reply_outcome(""), SetupOutcome::Failed(_)));
    }

    fn info(pairs: &[(&str, OwnedValue)]) -> HashMap<String, OwnedValue> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect()
    }

    #[test]
    fn an_empty_info_dictionary_means_the_extension_is_absent() {
        // GNOME answers an unknown uuid with an empty dictionary rather than an error.
        assert_eq!(
            extension_state_from_info(&HashMap::new()),
            ExtensionState::Absent
        );
    }

    #[test]
    fn extension_state_reads_a_double_or_an_integer() {
        // GNOME packs every JS number as a D-Bus double, so `d` is the real wire type
        // and a `u`-only reader gets nothing. Both are accepted rather than one pinned.
        let double = info(&[("state", OwnedValue::from(1.0_f64))]);
        assert_eq!(extension_state_from_info(&double), ExtensionState::Active);
        let integer = info(&[("state", OwnedValue::from(1_u32))]);
        assert_eq!(extension_state_from_info(&integer), ExtensionState::Active);
    }

    #[test]
    fn an_enabled_flag_does_not_override_a_state_that_is_not_running() {
        // `enabled` is the owner's intent, not the runtime. It reads true for an
        // extension that failed to load and for one GNOME is refusing to load at all.
        let errored = info(&[
            ("state", OwnedValue::from(3.0_f64)),
            ("enabled", OwnedValue::from(true)),
        ]);
        assert_eq!(extension_state_from_info(&errored), ExtensionState::Present);
    }

    #[test]
    fn a_failure_is_never_framed_as_gnome_declining_an_install() {
        // Three of the reasons that reach Failed are not an install at all, and one is
        // our own probe giving up. A "GNOME did not add the extension" prefix is wrong
        // over every one of them.
        for reason in [
            "GNOME is set not to load extensions, so the panel icon cannot come back this way",
            "GNOME would not turn the extension on",
            "could not reach the desktop to check what it needs",
        ] {
            let (message, code) = command_result(&SetupOutcome::Failed(reason.into()));
            assert_eq!(code, 1);
            assert!(message.contains(reason), "{reason}");
            assert!(!message.contains("did not add"), "{reason}");
            assert!(!message.contains("could not add"), "{reason}");
        }
    }

    #[test]
    fn no_owner_facing_string_carries_a_raw_extension_uuid() {
        // A uuid is not something the owner can act on; it belongs in the log.
        let strings = [
            OFFER_BODY.to_owned(),
            command_preamble(PanelIconReadiness::ExtensionOff).to_owned(),
            command_preamble(PanelIconReadiness::ExtensionMissing).to_owned(),
            command_preamble(PanelIconReadiness::Unknown).to_owned(),
            command_result(&SetupOutcome::NeedsRelogin).0,
            command_result(&SetupOutcome::NothingToSetUp).0,
            offer_result_body(&SetupOutcome::Installed).unwrap_or_default(),
        ];
        for value in strings {
            assert!(!value.contains('@'), "{value}");
            assert!(!value.contains(APPINDICATOR_UUID), "{value}");
        }
    }

    #[test]
    fn install_errors_become_reasons_an_owner_can_act_on() {
        let locked = install_error_reason(
            "org.gnome.Shell.Extensions.Error.NotAllowed",
            "unused fallback",
        );
        assert!(locked.contains("does not allow"));
        assert!(
            install_error_reason("org.gnome.Shell.Extensions.Error.DownloadFailed", "x")
                .contains("network")
        );
        assert!(
            install_error_reason("org.gnome.Shell.Extensions.Error.ExtractFailed", "x")
                .contains("unpacked")
        );
        assert!(
            install_error_reason("org.gnome.Shell.Extensions.Error.EnableFailed", "x")
                .contains("could not turn it on")
        );
        // An error we have never seen keeps its own text rather than being flattened.
        assert_eq!(
            install_error_reason("org.freedesktop.DBus.Error.ServiceUnknown", "raw detail"),
            "raw detail"
        );
        assert_eq!(install_error_reason("", "raw detail"), "raw detail");
    }

    #[test]
    fn every_non_active_state_is_present_not_active() {
        // 6 is INITIALIZED, which is what a freshly installed package reports and what
        // the old check called success.
        for value in [2.0_f64, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0] {
            let dictionary = info(&[("state", OwnedValue::from(value))]);
            assert_eq!(
                extension_state_from_info(&dictionary),
                ExtensionState::Present,
                "state {value}"
            );
        }
    }

    #[test]
    fn an_info_dictionary_without_a_state_key_still_counts_as_present() {
        let dictionary = info(&[("uuid", OwnedValue::from(0_u32))]);
        assert_eq!(
            extension_state_from_info(&dictionary),
            ExtensionState::Present
        );
    }

    #[test]
    fn setup_routes_each_state_to_the_call_that_fits_it() {
        // Routing only; the two bus calls are exercised on a real GNOME session.
        assert!(!PanelIconReadiness::Available.is_fixable());
        assert!(!PanelIconReadiness::NotApplicable.is_fixable());
        assert!(!PanelIconReadiness::ExtensionActiveNoHost.is_fixable());
        assert!(PanelIconReadiness::ExtensionOff.is_fixable());
        assert!(PanelIconReadiness::ExtensionMissing.is_fixable());
    }
}
