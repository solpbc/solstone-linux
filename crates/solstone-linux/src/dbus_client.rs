// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::time::Duration;
use zbus::Connection;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
pub const PAIRING_TIMEOUT: Duration = crate::window_control::PAIR_BOUND;
// The pair itself, plus the journal's mint and the app's own proof around it.
const CONNECT_LOCAL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ControlClientError {
    NoOwner,
    LostOwner,
    UnknownMethod,
    Timeout,
    Call(String),
}

impl std::fmt::Display for ControlClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOwner => write!(f, "service has no owner"),
            Self::LostOwner => write!(f, "service owner changed during call"),
            Self::UnknownMethod => write!(f, "method not supported by service"),
            Self::Timeout => write!(f, "call timed out"),
            Self::Call(msg) => write!(f, "dbus call failed: {msg}"),
        }
    }
}

impl std::error::Error for ControlClientError {}

impl From<zbus::Error> for ControlClientError {
    fn from(err: zbus::Error) -> Self {
        classify_zbus_error(&err)
    }
}

impl From<zbus::fdo::Error> for ControlClientError {
    fn from(err: zbus::fdo::Error) -> Self {
        classify_zbus_error(&zbus::Error::from(err))
    }
}

pub fn classify_zbus_error(err: &zbus::Error) -> ControlClientError {
    match err {
        zbus::Error::MethodError(name, _, _) => {
            let name_str = name.as_str();
            if name_str == "org.freedesktop.DBus.Error.UnknownMethod" {
                ControlClientError::UnknownMethod
            } else if name_str == "org.freedesktop.DBus.Error.NoReply"
                || name_str == "org.freedesktop.DBus.Error.Timeout"
            {
                ControlClientError::Timeout
            } else if name_str == "org.freedesktop.DBus.Error.ServiceUnknown"
                || name_str == "org.freedesktop.DBus.Error.NameHasNoOwner"
            {
                ControlClientError::NoOwner
            } else {
                ControlClientError::Call(name_str.to_string())
            }
        }
        zbus::Error::FDO(boxed_fdo) => match boxed_fdo.as_ref() {
            zbus::fdo::Error::UnknownMethod(_) => ControlClientError::UnknownMethod,
            zbus::fdo::Error::Timeout(_) => ControlClientError::Timeout,
            zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_) => {
                ControlClientError::NoOwner
            }
            other => ControlClientError::Call(other.to_string()),
        },
        other => ControlClientError::Call(other.to_string()),
    }
}

pub fn verify_owner(
    current_owner: Option<&str>,
    expected_owner: &str,
) -> Result<(), ControlClientError> {
    match current_owner {
        None => Err(ControlClientError::NoOwner),
        Some(owner) if owner == expected_owner => Ok(()),
        Some(_) => Err(ControlClientError::LostOwner),
    }
}

pub struct ControlClient {
    connection: Connection,
    expected_owner: Option<String>,
}

impl ControlClient {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            expected_owner: None,
        }
    }

    pub fn expect_owner(mut self, owner: String) -> Self {
        self.expected_owner = Some(owner);
        self
    }

    pub async fn session() -> Result<Self, ControlClientError> {
        let connection = tokio::time::timeout(DEFAULT_TIMEOUT, Connection::session())
            .await
            .map_err(|_| ControlClientError::Timeout)??;
        Ok(Self::new(connection))
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub async fn get_owner(&self) -> Result<String, ControlClientError> {
        tokio::time::timeout(DEFAULT_TIMEOUT, self.get_owner_inner())
            .await
            .map_err(|_| ControlClientError::Timeout)?
    }

    async fn get_owner_inner(&self) -> Result<String, ControlClientError> {
        let dbus_proxy = zbus::fdo::DBusProxy::new(&self.connection).await?;
        let name: zbus::names::BusName = crate::desktop_component::OBSERVER_BUS_NAME
            .try_into()
            .map_err(|e: zbus::names::Error| ControlClientError::Call(e.to_string()))?;
        match dbus_proxy.get_name_owner(name).await {
            Ok(owner) => Ok(owner.to_string()),
            Err(err) => Err(ControlClientError::from(err)),
        }
    }

    pub async fn get_connection_unix_process_id(
        &self,
        bus_name: &str,
    ) -> Result<u32, ControlClientError> {
        tokio::time::timeout(DEFAULT_TIMEOUT, self.get_pid_inner(bus_name))
            .await
            .map_err(|_| ControlClientError::Timeout)?
    }

    async fn get_pid_inner(&self, bus_name: &str) -> Result<u32, ControlClientError> {
        let dbus_proxy = zbus::fdo::DBusProxy::new(&self.connection).await?;
        let name: zbus::names::BusName = bus_name
            .try_into()
            .map_err(|e: zbus::names::Error| ControlClientError::Call(e.to_string()))?;
        match dbus_proxy.get_connection_unix_process_id(name).await {
            Ok(pid) => Ok(pid),
            Err(err) => Err(ControlClientError::from(err)),
        }
    }

    async fn call_method(
        &self,
        method: &str,
        body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
        timeout: Duration,
    ) -> Result<String, ControlClientError> {
        let initial_owner = self.get_owner().await?;
        if let Some(expected) = &self.expected_owner {
            verify_owner(Some(&initial_owner), expected)?;
        }

        let call_future = self.connection.call_method(
            Some(initial_owner.as_str()),
            crate::shell::OBSERVER_PATH,
            Some(crate::desktop_component::OBSERVER_BUS_NAME),
            method,
            body,
        );
        let reply = tokio::time::timeout(timeout, call_future)
            .await
            .map_err(|_| ControlClientError::Timeout)?
            .map_err(ControlClientError::from)?;

        let current_owner = self.get_owner().await;
        match current_owner {
            Ok(owner) => verify_owner(Some(&owner), &initial_owner)?,
            Err(_) => verify_owner(None, &initial_owner)?,
        }

        let result: String = reply
            .body()
            .deserialize()
            .map_err(ControlClientError::from)?;
        Ok(result)
    }

    pub async fn get_control_state(&self) -> Result<String, ControlClientError> {
        self.call_method("GetControlState", &(), DEFAULT_TIMEOUT)
            .await
    }

    pub async fn begin_pairing(&self, link: &str) -> Result<String, ControlClientError> {
        self.call_method("BeginPairing", &(link.to_string(),), PAIRING_TIMEOUT)
            .await
    }

    pub async fn local_journal_offer(&self) -> Result<String, ControlClientError> {
        self.call_method("LocalJournalOffer", &(), DEFAULT_TIMEOUT)
            .await
    }

    pub async fn connect_local_journal(
        &self,
        instance_id: &str,
    ) -> Result<String, ControlClientError> {
        self.call_method(
            "ConnectLocalJournal",
            &(instance_id.to_string(),),
            CONNECT_LOCAL_TIMEOUT,
        )
        .await
    }

    pub async fn answer_pairing(
        &self,
        attempt_id: &str,
        pairing_id: &str,
        accept: bool,
    ) -> Result<String, ControlClientError> {
        self.call_method(
            "AnswerPairing",
            &(attempt_id.to_string(), pairing_id.to_string(), accept),
            DEFAULT_TIMEOUT,
        )
        .await
    }

    pub async fn cancel_pairing(&self, attempt_id: &str) -> Result<String, ControlClientError> {
        self.call_method("CancelPairing", &(attempt_id.to_string(),), DEFAULT_TIMEOUT)
            .await
    }

    pub async fn write_setting(
        &self,
        key: &str,
        value: &str,
    ) -> Result<String, ControlClientError> {
        self.call_method(
            "WriteSetting",
            &(key.to_string(), value.to_string()),
            DEFAULT_TIMEOUT,
        )
        .await
    }

    pub async fn open_journal(&self) -> Result<String, ControlClientError> {
        self.call_method("OpenJournal", &(), DEFAULT_TIMEOUT).await
    }

    pub async fn obsidian_state(&self) -> Result<String, ControlClientError> {
        self.call_method("ObsidianState", &(), DEFAULT_TIMEOUT)
            .await
    }

    pub async fn obsidian_turn_on(&self, path: &str) -> Result<String, ControlClientError> {
        self.call_method("ObsidianTurnOn", &(path.to_string(),), DEFAULT_TIMEOUT)
            .await
    }

    pub async fn obsidian_turn_off(&self) -> Result<String, ControlClientError> {
        self.call_method("ObsidianTurnOff", &(), DEFAULT_TIMEOUT)
            .await
    }

    pub async fn obsidian_continue(&self) -> Result<String, ControlClientError> {
        self.call_method("ObsidianContinue", &(), DEFAULT_TIMEOUT)
            .await
    }

    pub async fn obsidian_cancel(&self) -> Result<String, ControlClientError> {
        self.call_method("ObsidianCancel", &(), DEFAULT_TIMEOUT)
            .await
    }

    pub async fn pause(&self, duration_seconds: i32) -> Result<String, ControlClientError> {
        self.call_method("Pause", &(duration_seconds,), DEFAULT_TIMEOUT)
            .await
    }

    pub async fn resume(&self) -> Result<String, ControlClientError> {
        self.call_method("Resume", &(), DEFAULT_TIMEOUT).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_message() -> zbus::message::Message {
        zbus::message::Message::method_call("/", "m")
            .unwrap()
            .build(&())
            .unwrap()
    }

    #[test]
    fn owner_verification_matches_current() {
        assert_eq!(verify_owner(Some(":1.42"), ":1.42"), Ok(()));
        assert_eq!(
            verify_owner(Some(":1.43"), ":1.42"),
            Err(ControlClientError::LostOwner)
        );
        assert_eq!(
            verify_owner(None, ":1.42"),
            Err(ControlClientError::NoOwner)
        );
    }

    #[test]
    fn error_classification_maps_known_dbus_errors() {
        let unknown = zbus::Error::MethodError(
            zbus::names::ErrorName::from_static_str("org.freedesktop.DBus.Error.UnknownMethod")
                .unwrap()
                .into(),
            None,
            dummy_message(),
        );
        assert_eq!(
            classify_zbus_error(&unknown),
            ControlClientError::UnknownMethod
        );

        let timeout = zbus::Error::MethodError(
            zbus::names::ErrorName::from_static_str("org.freedesktop.DBus.Error.Timeout")
                .unwrap()
                .into(),
            None,
            dummy_message(),
        );
        assert_eq!(classify_zbus_error(&timeout), ControlClientError::Timeout);

        let no_reply = zbus::Error::MethodError(
            zbus::names::ErrorName::from_static_str("org.freedesktop.DBus.Error.NoReply")
                .unwrap()
                .into(),
            None,
            dummy_message(),
        );
        assert_eq!(classify_zbus_error(&no_reply), ControlClientError::Timeout);

        let no_owner = zbus::Error::MethodError(
            zbus::names::ErrorName::from_static_str("org.freedesktop.DBus.Error.NameHasNoOwner")
                .unwrap()
                .into(),
            None,
            dummy_message(),
        );
        assert_eq!(classify_zbus_error(&no_owner), ControlClientError::NoOwner);

        let service_unknown = zbus::Error::MethodError(
            zbus::names::ErrorName::from_static_str("org.freedesktop.DBus.Error.ServiceUnknown")
                .unwrap()
                .into(),
            None,
            dummy_message(),
        );
        assert_eq!(
            classify_zbus_error(&service_unknown),
            ControlClientError::NoOwner
        );

        let fdo_no_owner = zbus::Error::from(zbus::fdo::Error::NameHasNoOwner("test".to_string()));
        assert_eq!(
            classify_zbus_error(&fdo_no_owner),
            ControlClientError::NoOwner
        );

        let fdo_service_unknown =
            zbus::Error::from(zbus::fdo::Error::ServiceUnknown("test".to_string()));
        assert_eq!(
            classify_zbus_error(&fdo_service_unknown),
            ControlClientError::NoOwner
        );

        let fdo_unknown = zbus::Error::from(zbus::fdo::Error::UnknownMethod("test".to_string()));
        assert_eq!(
            classify_zbus_error(&fdo_unknown),
            ControlClientError::UnknownMethod
        );

        let fdo_timeout = zbus::Error::from(zbus::fdo::Error::Timeout("test".to_string()));
        assert_eq!(
            classify_zbus_error(&fdo_timeout),
            ControlClientError::Timeout
        );
    }
}
