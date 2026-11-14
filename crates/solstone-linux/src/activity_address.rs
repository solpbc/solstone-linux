// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Committed document metadata only. No text or editable-value interface is used.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::activity_prefs::ActivityPrefs;

const MAX_NODES: usize = 256;
const MAX_DEPTH: usize = 16;
const FRAME: u32 = 23;
const DOCUMENTS: [u32; 2] = [82, 95];
const PASSWORD: u32 = 40;
const ACTIVE: u64 = 1 << 1;
const SHOWING: u64 = 1 << 25;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NodeRef(String, String);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Metadata {
    role: u32,
    states: u64,
    children: Vec<NodeRef>,
}

trait DocumentBus {
    async fn applications(&mut self) -> Option<Vec<(NodeRef, u32)>>;
    async fn metadata(&mut self, node: &NodeRef) -> Option<Metadata>;
    async fn doc_url(&mut self, node: &NodeRef) -> Option<String>;
}

async fn scan(
    bus: &mut impl DocumentBus,
    root: &NodeRef,
) -> Option<(BTreeMap<NodeRef, Metadata>, NodeRef)> {
    let mut tree = BTreeMap::new();
    let mut pending = vec![(root.clone(), 0, false)];
    let mut frames = 0;
    let mut documents = Vec::new();
    while let Some((node, depth, in_frame)) = pending.pop() {
        if depth > MAX_DEPTH || tree.len() >= MAX_NODES || tree.contains_key(&node) {
            return None;
        }
        let info = bus.metadata(&node).await?;
        let is_frame = info.role == FRAME;
        let active = info.states & (ACTIVE | SHOWING) == (ACTIVE | SHOWING);
        let in_frame = if is_frame { active } else { in_frame };
        if is_frame {
            frames += 1;
        }
        if DOCUMENTS.contains(&info.role) && in_frame && info.states & SHOWING != 0 {
            documents.push(node.clone());
        }
        // Document roots and password controls are terminal. Inactive frames
        // cannot contain the foreground page and need no descendant reads.
        if !DOCUMENTS.contains(&info.role) && info.role != PASSWORD && (!is_frame || active) {
            pending.extend(
                info.children
                    .iter()
                    .cloned()
                    .map(|child| (child, depth + 1, in_frame)),
            );
        }
        tree.insert(node, info);
    }
    if frames != 1 || documents.len() != 1 {
        return None;
    }
    Some((tree, documents.pop()?))
}

async fn committed_document(bus: &mut impl DocumentBus, pid: u32) -> Option<String> {
    let apps = bus.applications().await?;
    let mut matching = apps.into_iter().filter(|(_, app_pid)| *app_pid == pid);
    let root = matching.next()?.0;
    if matching.next().is_some() {
        return None;
    }
    let (before, document) = scan(bus, &root).await?;
    let url = bus.doc_url(&document).await?;
    if url.len() > crate::activity_writer::MAX_STRING_BYTES {
        return None;
    }
    let (after, same_document) = scan(bus, &root).await?;
    // The value came from the selected object, never a newly selected node in
    // the second walk. A changed or incomplete tree invalidates the value.
    (document == same_document && before == after).then_some(url)
}

struct NativeDocumentBus {
    connection: zbus::Connection,
    prefs: Arc<ActivityPrefs>,
    generation: u64,
}

impl NativeDocumentBus {
    fn allowed(&self) -> bool {
        let prefs = self.prefs.snapshot();
        prefs.source && prefs.urls && prefs.generation == self.generation
    }

    async fn children(&self, node: &NodeRef) -> Option<Vec<NodeRef>> {
        if !self.allowed() {
            return None;
        }
        let reply = self
            .connection
            .call_method(
                Some(node.0.as_str()),
                node.1.as_str(),
                Some("org.a11y.atspi.Accessible"),
                "GetChildren",
                &(),
            )
            .await
            .ok()?;
        if reply.body().len() > 65536 {
            return None;
        }
        let refs: Vec<(String, zbus::zvariant::OwnedObjectPath)> =
            reply.body().deserialize().ok()?;
        if refs.len() > MAX_NODES {
            return None;
        }
        refs.into_iter()
            .map(|(bus, path)| {
                if path.as_str() == "/org/a11y/atspi/null" {
                    None
                } else {
                    Some(NodeRef(
                        if bus.is_empty() { node.0.clone() } else { bus },
                        path.to_string(),
                    ))
                }
            })
            .collect()
    }
}

impl DocumentBus for NativeDocumentBus {
    async fn applications(&mut self) -> Option<Vec<(NodeRef, u32)>> {
        let root = NodeRef(
            "org.a11y.atspi.Registry".into(),
            "/org/a11y/atspi/accessible/root".into(),
        );
        let refs = self.children(&root).await?;
        let mut result = Vec::new();
        for node in refs {
            if !self.allowed() {
                return None;
            }
            let reply = self
                .connection
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "GetConnectionUnixProcessID",
                    &(node.0.as_str(),),
                )
                .await
                .ok()?;
            result.push((node, reply.body().deserialize().ok()?));
        }
        Some(result)
    }

    async fn metadata(&mut self, node: &NodeRef) -> Option<Metadata> {
        if !self.allowed() {
            return None;
        }
        let role: u32 = self
            .connection
            .call_method(
                Some(node.0.as_str()),
                node.1.as_str(),
                Some("org.a11y.atspi.Accessible"),
                "GetRole",
                &(),
            )
            .await
            .ok()?
            .body()
            .deserialize()
            .ok()?;
        if !self.allowed() {
            return None;
        }
        let states: Vec<u32> = self
            .connection
            .call_method(
                Some(node.0.as_str()),
                node.1.as_str(),
                Some("org.a11y.atspi.Accessible"),
                "GetState",
                &(),
            )
            .await
            .ok()?
            .body()
            .deserialize()
            .ok()?;
        if states.len() != 2 {
            return None;
        }
        let states = u64::from(states[0]) | (u64::from(states[1]) << 32);
        let children = if DOCUMENTS.contains(&role)
            || role == PASSWORD
            || (role == FRAME && states & (ACTIVE | SHOWING) != (ACTIVE | SHOWING))
        {
            Vec::new()
        } else {
            self.children(node).await?
        };
        Some(Metadata {
            role,
            states,
            children,
        })
    }

    async fn doc_url(&mut self, node: &NodeRef) -> Option<String> {
        if !self.allowed() {
            return None;
        }
        self.connection
            .call_method(
                Some(node.0.as_str()),
                node.1.as_str(),
                Some("org.a11y.atspi.Document"),
                "GetAttributeValue",
                &("DocURL",),
            )
            .await
            .ok()?
            .body()
            .deserialize()
            .ok()
    }
}

pub trait AddressReader: Send {
    fn read(&mut self, pid: u32, prefs: Arc<ActivityPrefs>) -> Option<String>;
}

type ReadRequest = (u32, Arc<ActivityPrefs>, mpsc::SyncSender<Option<String>>);

pub struct NativeAddressReader {
    sender: mpsc::SyncSender<ReadRequest>,
}

impl NativeAddressReader {
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<ReadRequest>(0);
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            for (pid, prefs, reply) in receiver {
                let generation = prefs.generation();
                let value = runtime.block_on(async {
                    tokio::time::timeout(Duration::from_millis(1500), async {
                        let snap = prefs.snapshot();
                        if !snap.source || !snap.urls {
                            return None;
                        }
                        let session = zbus::Connection::session().await.ok()?;
                        let address: String = session
                            .call_method(
                                Some("org.a11y.Bus"),
                                "/org/a11y/bus",
                                Some("org.a11y.Bus"),
                                "GetAddress",
                                &(),
                            )
                            .await
                            .ok()?
                            .body()
                            .deserialize()
                            .ok()?;
                        let connection = zbus::connection::Builder::address(address.as_str())
                            .ok()?
                            .build()
                            .await
                            .ok()?;
                        committed_document(
                            &mut NativeDocumentBus {
                                connection,
                                prefs,
                                generation,
                            },
                            pid,
                        )
                        .await
                    })
                    .await
                    .ok()
                    .flatten()
                });
                let _ = reply.send(value);
            }
        });
        Self { sender }
    }
}

impl AddressReader for NativeAddressReader {
    fn read(&mut self, pid: u32, prefs: Arc<ActivityPrefs>) -> Option<String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender.try_send((pid, prefs, sender)).ok()?;
        receiver
            .recv_timeout(Duration::from_millis(1750))
            .ok()
            .flatten()
    }
}

trait AccessibilitySetting {
    fn enabled(&mut self) -> Option<bool>;
    fn set_enabled(&mut self, enabled: bool) -> Option<()>;
    fn owned(&self) -> bool;
    fn save_owned(&mut self, owned: bool) -> Option<()>;
}

fn reconcile(setting: &mut impl AccessibilitySetting, wanted: bool) -> Option<()> {
    if !wanted && !setting.owned() {
        return Some(());
    }
    let enabled = setting.enabled()?;
    if wanted {
        if !enabled {
            setting.set_enabled(true)?;
            if setting.save_owned(true).is_none() {
                setting.set_enabled(false)?;
                return None;
            }
        }
    } else {
        if enabled {
            setting.set_enabled(false)?;
        }
        setting.save_owned(false)?;
    }
    Some(())
}

struct NativeSetting {
    marker: PathBuf,
}

impl AccessibilitySetting for NativeSetting {
    fn enabled(&mut self) -> Option<bool> {
        let conn = zbus::blocking::connection::Builder::session()
            .ok()?
            .method_timeout(Duration::from_millis(500))
            .build()
            .ok()?;
        zbus::blocking::Proxy::new(&conn, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Status")
            .ok()?
            .get_property("IsEnabled")
            .ok()
    }
    fn set_enabled(&mut self, enabled: bool) -> Option<()> {
        let conn = zbus::blocking::connection::Builder::session()
            .ok()?
            .method_timeout(Duration::from_millis(500))
            .build()
            .ok()?;
        zbus::blocking::Proxy::new(&conn, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Status")
            .ok()?
            .set_property("IsEnabled", enabled)
            .ok()
    }
    fn owned(&self) -> bool {
        self.marker.is_file()
    }
    fn save_owned(&mut self, owned: bool) -> Option<()> {
        if owned {
            std::fs::create_dir_all(self.marker.parent()?).ok()?;
            if !self.owned() {
                std::fs::File::create_new(&self.marker)
                    .ok()?
                    .sync_all()
                    .ok()?;
            }
        } else {
            match std::fs::remove_file(&self.marker) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
        }
        Some(())
    }
}

/// The first retry after the accessibility setting couldn't be reached; each one
/// after waits twice as long, up to [`RETRY_LONGEST`].
const RETRY_FIRST: Duration = Duration::from_millis(500);
const RETRY_LONGEST: Duration = Duration::from_secs(30);

pub fn follow_permission(prefs: Arc<ActivityPrefs>, state_dir: PathBuf) {
    std::thread::spawn(move || {
        let mut setting = NativeSetting {
            marker: state_dir.join("activity-address-accessibility-owned"),
        };
        follow(&mut setting, &prefs, |seen, timeout| {
            prefs.wait_policy_change(seen, timeout);
            true
        });
    });
}

/// Keep the setting where the owner's choice wants it. Once it is there, sleep until
/// the choice changes; while it can't be reached, retry with a growing wait.
fn follow(
    setting: &mut impl AccessibilitySetting,
    prefs: &ActivityPrefs,
    mut wait: impl FnMut(u64, Option<Duration>) -> bool,
) {
    let mut applied = None;
    let mut retry: Option<Duration> = None;
    loop {
        let seen = prefs.policy_changes();
        let wanted = prefs.snapshot().urls;
        if applied != Some(wanted) {
            if reconcile(setting, wanted).is_some() {
                applied = Some(wanted);
                retry = None;
            } else {
                retry = Some(retry.map_or(RETRY_FIRST, |last| (last * 2).min(RETRY_LONGEST)));
            }
        }
        if !wait(seen, retry) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        tree: BTreeMap<NodeRef, Metadata>,
        reads: usize,
        change: bool,
    }
    fn node(n: &str) -> NodeRef {
        NodeRef(":1.2".into(), n.into())
    }
    fn fixture() -> Fake {
        Fake {
            tree: BTreeMap::from([
                (
                    node("root"),
                    Metadata {
                        role: 75,
                        states: 0,
                        children: vec![node("frame")],
                    },
                ),
                (
                    node("frame"),
                    Metadata {
                        role: FRAME,
                        states: ACTIVE | SHOWING,
                        children: vec![node("doc")],
                    },
                ),
                (
                    node("doc"),
                    Metadata {
                        role: 95,
                        states: SHOWING,
                        children: Vec::new(),
                    },
                ),
            ]),
            reads: 0,
            change: false,
        }
    }
    impl DocumentBus for Fake {
        async fn applications(&mut self) -> Option<Vec<(NodeRef, u32)>> {
            Some(vec![(node("root"), 9)])
        }
        async fn metadata(&mut self, n: &NodeRef) -> Option<Metadata> {
            self.tree.get(n).cloned()
        }
        async fn doc_url(&mut self, _: &NodeRef) -> Option<String> {
            self.reads += 1;
            if self.change {
                self.tree.get_mut(&node("frame"))?.states = SHOWING;
            }
            Some("https://user:secret@example.com/path?q=private#fragment".into())
        }
    }
    #[tokio::test]
    async fn stable_document_and_changing_tree_twin() {
        let mut bus = fixture();
        let url = committed_document(&mut bus, 9).await.unwrap();
        assert_eq!(
            crate::activity_writer::sanitize_url(&url).as_deref(),
            Some("https://example.com/path")
        );
        assert_eq!(bus.reads, 1);
        let mut bus = fixture();
        bus.change = true;
        assert!(committed_document(&mut bus, 9).await.is_none());
        assert_eq!(bus.reads, 1);
    }

    #[tokio::test]
    async fn inactive_second_frame_prevents_pid_only_attribution() {
        let mut bus = fixture();
        bus.tree
            .get_mut(&node("root"))
            .unwrap()
            .children
            .push(node("inactive-frame"));
        bus.tree.insert(
            node("inactive-frame"),
            Metadata {
                role: FRAME,
                states: SHOWING,
                children: Vec::new(),
            },
        );
        assert!(committed_document(&mut bus, 9).await.is_none());
        assert_eq!(bus.reads, 0);
        bus.tree.get_mut(&node("root")).unwrap().children.pop();
        assert!(committed_document(&mut bus, 9).await.is_some());
        assert_eq!(bus.reads, 1);
    }
    #[tokio::test]
    async fn ambiguous_or_incomplete_trees_never_read_document_values() {
        let mut bus = fixture();
        bus.tree
            .get_mut(&node("frame"))
            .unwrap()
            .children
            .push(node("another"));
        bus.tree.insert(
            node("another"),
            Metadata {
                role: 95,
                states: SHOWING,
                children: Vec::new(),
            },
        );
        assert!(committed_document(&mut bus, 9).await.is_none());
        assert_eq!(bus.reads, 0);
        let mut bus = fixture();
        bus.tree
            .get_mut(&node("frame"))
            .unwrap()
            .children
            .push(node("missing"));
        assert!(committed_document(&mut bus, 9).await.is_none());
        assert_eq!(bus.reads, 0);
        let mut bus = fixture();
        let leaves: Vec<_> = (0..MAX_NODES).map(|i| node(&format!("leaf-{i}"))).collect();
        for leaf in &leaves {
            bus.tree.insert(
                leaf.clone(),
                Metadata {
                    role: 39,
                    states: 0,
                    children: Vec::new(),
                },
            );
        }
        let mut children = leaves;
        // Stack order discovers the valid document first. Exhausting the
        // remaining complete metadata must still reject apparent uniqueness.
        children.push(node("frame"));
        bus.tree.get_mut(&node("root")).unwrap().children = children;
        assert!(committed_document(&mut bus, 9).await.is_none());
        assert_eq!(bus.reads, 0);
    }
    #[derive(Default)]
    struct Setting {
        enabled: bool,
        owned: bool,
        calls: Vec<bool>,
        fail: bool,
    }
    impl AccessibilitySetting for Setting {
        fn enabled(&mut self) -> Option<bool> {
            Some(self.enabled)
        }
        fn set_enabled(&mut self, v: bool) -> Option<()> {
            self.calls.push(v);
            if self.fail {
                return None;
            }
            self.enabled = v;
            Some(())
        }
        fn owned(&self) -> bool {
            self.owned
        }
        fn save_owned(&mut self, v: bool) -> Option<()> {
            self.owned = v;
            Some(())
        }
    }
    #[test]
    fn permission_consent_and_restart_ownership() {
        let mut s = Setting::default();
        reconcile(&mut s, false).unwrap();
        assert!(s.calls.is_empty());
        reconcile(&mut s, true).unwrap();
        assert!(s.enabled && s.owned);
        let mut restarted = Setting {
            enabled: s.enabled,
            owned: s.owned,
            ..Setting::default()
        };
        reconcile(&mut restarted, false).unwrap();
        assert!(!restarted.enabled && !restarted.owned);
        let mut external = Setting {
            enabled: true,
            ..Setting::default()
        };
        reconcile(&mut external, true).unwrap();
        reconcile(&mut external, false).unwrap();
        assert!(external.enabled && external.calls.is_empty());
        let mut failed = Setting {
            fail: true,
            ..Setting::default()
        };
        assert!(reconcile(&mut failed, true).is_none());
        assert!(!failed.owned);
    }

    #[test]
    fn the_follower_sleeps_until_a_change_and_backs_off_while_unreachable() {
        let config = crate::config::Config {
            activity_urls: true,
            ..crate::config::Config::default()
        };
        let prefs = ActivityPrefs::new(&config);
        let mut waits = Vec::new();
        let mut reached = Setting::default();
        follow(&mut reached, &prefs, |_, timeout| {
            waits.push(timeout);
            waits.len() < 5
        });
        assert!(reached.enabled);
        assert_eq!(waits, vec![None; 5], "an applied choice polled again");

        let mut unreachable = Setting {
            fail: true,
            ..Setting::default()
        };
        let mut retries = Vec::new();
        follow(&mut unreachable, &prefs, |_, timeout| {
            retries.push(timeout.expect("a failed reach is retried"));
            retries.len() < 10
        });
        assert!(retries.windows(2).all(|pair| pair[1] >= pair[0]));
        assert!(retries[0] >= Duration::from_millis(500));
        assert_eq!(retries[9], RETRY_LONGEST);
    }
}
