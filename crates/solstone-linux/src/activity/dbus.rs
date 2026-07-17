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
    ActivityLogLevel, ActivityOps, BackendOutcome, BoundBackends, CacheState, DBUS_TIMEOUT,
    LockSignal, ScreenSaverSpec, SessionBusOps, SystemBusOps, probe_backends, resolve,
    wayland_idle::NativeWaylandIdle, x11::NativeX11,
};
use crate::observer::ActivityState;

trait ScreenSaverProxyOps {
    type Proxy: Clone;
    fn construct(&mut self, spec: &ScreenSaverSpec) -> BackendOutcome<Self::Proxy>;
    fn get_active(&mut self, proxy: &Self::Proxy) -> BackendOutcome<bool>;
}

struct CachedScreenSavers<O: ScreenSaverProxyOps> {
    ops: O,
    proxies: HashMap<&'static str, O::Proxy>,
}

impl<O: ScreenSaverProxyOps> CachedScreenSavers<O> {
    fn get_active(&mut self, spec: &ScreenSaverSpec) -> BackendOutcome<bool> {
        let proxy = if let Some(proxy) = self.proxies.get(spec.key) {
            proxy.clone()
        } else {
            match self.ops.construct(spec) {
                BackendOutcome::Available(proxy) => {
                    self.proxies.insert(spec.key, proxy.clone());
                    proxy
                }
                BackendOutcome::Absent => return BackendOutcome::Absent,
                BackendOutcome::Broken(error) => return BackendOutcome::Broken(error),
            }
        };
        let result = self.ops.get_active(&proxy);
        if matches!(result, BackendOutcome::Absent | BackendOutcome::Broken(_)) {
            self.proxies.remove(spec.key);
        }
        result
    }
}

struct NativeScreenSaverProxyOps {
    runtime: Arc<tokio::runtime::Runtime>,
    connection: Option<Connection>,
}

impl ScreenSaverProxyOps for NativeScreenSaverProxyOps {
    type Proxy = Proxy<'static>;

    fn construct(&mut self, spec: &ScreenSaverSpec) -> BackendOutcome<Self::Proxy> {
        let Some(connection) = &self.connection else {
            return BackendOutcome::Absent;
        };
        self.runtime.block_on(timeout(Proxy::new_owned(
            connection.clone(),
            spec.bus,
            spec.path,
            spec.bus,
        )))
    }

    fn get_active(&mut self, proxy: &Self::Proxy) -> BackendOutcome<bool> {
        self.runtime
            .block_on(timeout(proxy.call::<_, _, bool>("GetActive", &())))
    }
}

struct NativeSessionBus {
    runtime: Arc<tokio::runtime::Runtime>,
    connection: Option<Connection>,
    screensavers: CachedScreenSavers<NativeScreenSaverProxyOps>,
}

impl NativeSessionBus {
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
        self.screensavers.get_active(spec)
    }

    fn name_has_owner(&mut self, bus: &'static str) -> BackendOutcome<bool> {
        let Some(connection) = &self.connection else {
            return BackendOutcome::Absent;
        };
        self.runtime.block_on(timeout(async {
            let proxy = zbus::fdo::DBusProxy::new(connection).await?;
            proxy
                .name_has_owner(zbus::names::BusName::try_from(bus)?)
                .await
                .map_err(Into::into)
        }))
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

pub struct NativeActivitySources {
    desktop: String,
    session_type: String,
    session: NativeSessionBus,
    system: NativeSystemBus,
    wayland: NativeWaylandIdle,
    x11: NativeX11,
    cache: CacheState,
    inventory: Option<BoundBackends>,
}

impl NativeActivitySources {
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
                connection: session_connection.clone(),
                screensavers: CachedScreenSavers {
                    ops: NativeScreenSaverProxyOps {
                        runtime: Arc::clone(&runtime),
                        connection: session_connection,
                    },
                    proxies: HashMap::new(),
                },
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
            inventory: None,
        }
    }
}

impl ActivityOps for NativeActivitySources {
    fn probe_once(&mut self) -> (ActivityState, BoundBackends) {
        if self.inventory.is_none() {
            let (inventory, warnings) = probe_backends(
                &self.desktop,
                &self.session_type,
                &mut self.session,
                &mut self.system,
                &mut self.wayland,
                &mut self.x11,
            );
            for warning in warnings {
                tracing::warn!("{warning}");
            }
            self.inventory = Some(inventory);
        }
        let inventory = self.inventory.expect("inventory initialized above");
        let result = resolve(
            &self.desktop,
            &self.session_type,
            &mut self.session,
            &mut self.system,
            &mut self.wayland,
            &mut self.x11,
            &mut self.cache,
        );
        for warning in self.cache.take_warnings() {
            emit_activity_log(warning);
        }
        (result.0, inventory)
    }
}

fn emit_activity_log(entry: super::ActivityLog) {
    match activity_log_level(&entry) {
        tracing::Level::DEBUG => tracing::debug!("{}", entry.message),
        _ => tracing::warn!("{}", entry.message),
    }
}

fn activity_log_level(entry: &super::ActivityLog) -> tracing::Level {
    match entry.level {
        ActivityLogLevel::Debug => tracing::Level::DEBUG,
        ActivityLogLevel::Warning => tracing::Level::WARN,
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
    match error {
        zbus::Error::MethodError(name, _, _) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
        ),
        zbus::Error::FDO(error) => matches!(
            error.as_ref(),
            zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, collections::VecDeque, rc::Rc};

    struct FakeProxyOps {
        constructions: Rc<Cell<usize>>,
        calls: VecDeque<BackendOutcome<bool>>,
    }

    impl ScreenSaverProxyOps for FakeProxyOps {
        type Proxy = ();
        fn construct(&mut self, _: &ScreenSaverSpec) -> BackendOutcome<Self::Proxy> {
            self.constructions.set(self.constructions.get() + 1);
            BackendOutcome::Available(())
        }
        fn get_active(&mut self, _: &Self::Proxy) -> BackendOutcome<bool> {
            self.calls.pop_front().expect("scripted proxy call")
        }
    }

    #[test]
    fn native_cache_layer_reuses_and_invalidates_proxy() {
        // tests/test_activity.py::TestIsScreenLocked::test_is_screen_locked_caches_and_invalidates_same_bus
        let constructions = Rc::new(Cell::new(0));
        let mut cache = CachedScreenSavers {
            ops: FakeProxyOps {
                constructions: Rc::clone(&constructions),
                calls: [
                    BackendOutcome::Available(false),
                    BackendOutcome::Available(false),
                    BackendOutcome::Broken("NoReply".into()),
                    BackendOutcome::Available(false),
                ]
                .into(),
            },
            proxies: HashMap::new(),
        };
        assert!(matches!(
            cache.get_active(&super::super::FDO),
            BackendOutcome::Available(false)
        ));
        assert!(matches!(
            cache.get_active(&super::super::FDO),
            BackendOutcome::Available(false)
        ));
        assert_eq!(constructions.get(), 1);
        assert!(matches!(
            cache.get_active(&super::super::FDO),
            BackendOutcome::Broken(_)
        ));
        assert!(matches!(
            cache.get_active(&super::super::FDO),
            BackendOutcome::Available(false)
        ));
        assert_eq!(constructions.get(), 2);
    }

    #[test]
    fn service_missing_uses_structured_error_names() {
        assert!(service_missing(&zbus::Error::FDO(Box::new(
            zbus::fdo::Error::ServiceUnknown("missing".into()),
        ))));
        assert!(service_missing(&zbus::Error::FDO(Box::new(
            zbus::fdo::Error::NameHasNoOwner("missing".into()),
        ))));
        assert!(!service_missing(&zbus::Error::FDO(Box::new(
            zbus::fdo::Error::NoReply("broken".into()),
        ))));
    }

    #[test]
    fn repeated_power_failure_uses_debug_after_warning() {
        assert_eq!(
            activity_log_level(&super::super::ActivityLog {
                level: ActivityLogLevel::Warning,
                message: "first".into()
            }),
            tracing::Level::WARN
        );
        assert_eq!(
            activity_log_level(&super::super::ActivityLog {
                level: ActivityLogLevel::Debug,
                message: "repeat".into()
            }),
            tracing::Level::DEBUG
        );
    }
}
