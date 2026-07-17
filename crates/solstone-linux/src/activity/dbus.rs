// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, mpsc},
};

use futures_util::StreamExt;
use zbus::{Connection, Proxy, fdo::PropertiesProxy};

use super::{
    ActivityOps, BackendOutcome, BoundBackends, CacheState, DBUS_TIMEOUT, LockSignal,
    ScreenSaverSpec, SessionBusOps, SystemBusOps, resolve, wayland_idle::NativeWaylandIdle,
    x11::NativeX11,
};
use crate::observer::ActivityState;

struct NativeSessionBus {
    runtime: Arc<tokio::runtime::Runtime>,
    connection: Option<Connection>,
    proxies: HashMap<&'static str, Proxy<'static>>,
}

impl NativeSessionBus {
    fn get_proxy(&mut self, spec: &ScreenSaverSpec) -> BackendOutcome<Proxy<'static>> {
        if let Some(proxy) = self.proxies.get(spec.key) {
            return BackendOutcome::Available(proxy.clone());
        }
        let Some(connection) = &self.connection else {
            return BackendOutcome::Absent;
        };
        let result = self.runtime.block_on(timeout(Proxy::new_owned(
            connection.clone(),
            spec.bus,
            spec.path,
            spec.bus,
        )));
        if let BackendOutcome::Available(proxy) = &result {
            self.proxies.insert(spec.key, proxy.clone());
        }
        result
    }

    fn property_i32(
        &self,
        destination: &'static str,
        path: &'static str,
        interface: &'static str,
        property: &'static str,
    ) -> BackendOutcome<i32> {
        let Some(connection) = &self.connection else {
            return BackendOutcome::Absent;
        };
        self.runtime.block_on(timeout(async {
            let proxy = PropertiesProxy::builder(connection)
                .destination(destination)?
                .path(path)?
                .build()
                .await?;
            let interface = zbus::names::InterfaceName::try_from(interface)?;
            let value = proxy.get(interface, property).await?;
            Ok(value.try_into()?)
        }))
    }
}

impl SessionBusOps for NativeSessionBus {
    fn get_active(&mut self, spec: &ScreenSaverSpec) -> BackendOutcome<bool> {
        let proxy = match self.get_proxy(spec) {
            BackendOutcome::Available(proxy) => proxy,
            BackendOutcome::Absent => return BackendOutcome::Absent,
            BackendOutcome::Broken(error) => return BackendOutcome::Broken(error),
        };
        let result = self
            .runtime
            .block_on(timeout(proxy.call::<_, _, bool>("GetActive", &())));
        if matches!(result, BackendOutcome::Absent | BackendOutcome::Broken(_)) {
            self.proxies.remove(spec.key);
        }
        result
    }

    fn mutter_power_mode(&mut self) -> BackendOutcome<i32> {
        self.property_i32(
            "org.gnome.Mutter.DisplayConfig",
            "/org/gnome/Mutter/DisplayConfig",
            "org.gnome.Mutter.DisplayConfig",
            "PowerSaveMode",
        )
    }

    fn mutter_idletime_ms(&mut self) -> BackendOutcome<u64> {
        let Some(connection) = &self.connection else {
            return BackendOutcome::Absent;
        };
        self.runtime.block_on(timeout(async {
            let proxy = Proxy::new(
                connection,
                "org.gnome.Mutter.IdleMonitor",
                "/org/gnome/Mutter/IdleMonitor/Core",
                "org.gnome.Mutter.IdleMonitor",
            )
            .await?;
            proxy.call::<_, _, u64>("GetIdletime", &()).await
        }))
    }
}

struct NativeSystemBus {
    runtime: Arc<tokio::runtime::Runtime>,
    connection: Option<Connection>,
    session_path: Option<zbus::zvariant::OwnedObjectPath>,
    signals_tx: mpsc::Sender<LockSignal>,
    signals_rx: mpsc::Receiver<LockSignal>,
}

impl NativeSystemBus {
    fn resolve_path(&mut self) -> BackendOutcome<zbus::zvariant::OwnedObjectPath> {
        if let Some(path) = &self.session_path {
            return BackendOutcome::Available(path.clone());
        }
        let Some(connection) = &self.connection else {
            return BackendOutcome::Absent;
        };
        let result: BackendOutcome<zbus::zvariant::OwnedObjectPath> =
            self.runtime.block_on(timeout(async {
                let proxy = Proxy::new(
                    connection,
                    "org.freedesktop.login1",
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                )
                .await?;
                proxy
                    .call::<_, _, zbus::zvariant::OwnedObjectPath>(
                        "GetSessionByPID",
                        &(std::process::id(),),
                    )
                    .await
            }));
        if let BackendOutcome::Available(path) = &result {
            self.session_path = Some(path.clone());
        }
        result
    }
}

impl SystemBusOps for NativeSystemBus {
    fn subscribe(&mut self) -> Result<(), String> {
        let path = match self.resolve_path() {
            BackendOutcome::Available(path) => path,
            BackendOutcome::Absent => return Err("system bus or logind session unavailable".into()),
            BackendOutcome::Broken(error) => return Err(error),
        };
        let connection = self.connection.clone().ok_or("system bus unavailable")?;
        let sender = self.signals_tx.clone();
        let (setup_tx, setup_rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let proxy = match Proxy::new_owned(connection, "org.freedesktop.login1", path, "org.freedesktop.login1.Session").await {
                Ok(proxy) => proxy,
                Err(error) => { let _ = setup_tx.send(Err(error.to_string())); return; }
            };
            let (mut locks, mut unlocks) = match (proxy.receive_signal("Lock").await, proxy.receive_signal("Unlock").await) {
                (Ok(locks), Ok(unlocks)) => (locks, unlocks),
                (Err(error), _) | (_, Err(error)) => { let _ = setup_tx.send(Err(error.to_string())); return; }
            };
            let _ = setup_tx.send(Ok(()));
            loop {
                tokio::select! {
                    value = locks.next() => if value.is_some() { let _ = sender.send(LockSignal::Lock); } else { break },
                    value = unlocks.next() => if value.is_some() { let _ = sender.send(LockSignal::Unlock); } else { break },
                }
            }
        });
        self.runtime
            .block_on(setup_rx)
            .map_err(|_| "logind subscription task stopped during setup".to_owned())?
    }

    fn locked_hint(&mut self) -> BackendOutcome<bool> {
        let path = match self.resolve_path() {
            BackendOutcome::Available(path) => path,
            BackendOutcome::Absent => return BackendOutcome::Absent,
            BackendOutcome::Broken(error) => return BackendOutcome::Broken(error),
        };
        let Some(connection) = &self.connection else {
            return BackendOutcome::Absent;
        };
        self.runtime.block_on(timeout(async {
            let proxy = PropertiesProxy::builder(connection)
                .destination("org.freedesktop.login1")?
                .path(path)?
                .build()
                .await?;
            let interface = zbus::names::InterfaceName::try_from("org.freedesktop.login1.Session")?;
            let value = proxy.get(interface, "LockedHint").await?;
            Ok(value.try_into()?)
        }))
    }

    fn drain_lock_signals(&mut self) -> Vec<LockSignal> {
        self.signals_rx.try_iter().collect()
    }
}

pub struct NativeActivityOps {
    desktop: String,
    session_type: String,
    session: NativeSessionBus,
    system: NativeSystemBus,
    wayland: NativeWaylandIdle,
    x11: NativeX11,
    cache: CacheState,
    emitted_warnings: usize,
}

impl NativeActivityOps {
    pub fn new(desktop: String, session_type: String) -> Self {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("activity runtime"),
        );
        let session_connection = runtime.block_on(Connection::session()).ok();
        let system_connection = runtime.block_on(Connection::system()).ok();
        let (signals_tx, signals_rx) = mpsc::channel();
        Self {
            desktop,
            session_type,
            session: NativeSessionBus {
                runtime: Arc::clone(&runtime),
                connection: session_connection,
                proxies: HashMap::new(),
            },
            system: NativeSystemBus {
                runtime,
                connection: system_connection,
                session_path: None,
                signals_tx,
                signals_rx,
            },
            wayland: NativeWaylandIdle::new(),
            x11: NativeX11::new(),
            cache: CacheState::default(),
            emitted_warnings: 0,
        }
    }
}

impl ActivityOps for NativeActivityOps {
    fn probe_once(&mut self) -> (ActivityState, BoundBackends) {
        let result = resolve(
            &self.desktop,
            &self.session_type,
            &mut self.session,
            &mut self.system,
            &mut self.wayland,
            &mut self.x11,
            &mut self.cache,
        );
        for warning in &self.cache.warnings[self.emitted_warnings..] {
            if warning.starts_with("DEBUG:") {
                tracing::debug!("{warning}");
            } else {
                tracing::warn!("{warning}");
            }
        }
        self.emitted_warnings = self.cache.warnings.len();
        result
    }
}

async fn timeout<T>(future: impl Future<Output = zbus::Result<T>>) -> BackendOutcome<T> {
    match tokio::time::timeout(DBUS_TIMEOUT, future).await {
        Ok(Ok(value)) => BackendOutcome::Available(value),
        Ok(Err(error)) if service_missing(&error) => BackendOutcome::Absent,
        Ok(Err(error)) => BackendOutcome::Broken(error.to_string()),
        Err(_) => BackendOutcome::Broken(format!("timed out after {}s", DBUS_TIMEOUT.as_secs())),
    }
}

fn service_missing(error: &zbus::Error) -> bool {
    let text = error.to_string();
    text.contains("org.freedesktop.DBus.Error.ServiceUnknown")
        || text.contains("org.freedesktop.DBus.Error.NameHasNoOwner")
}
