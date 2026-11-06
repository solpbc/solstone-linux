// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Finds the journal running under this OS account, through its owner socket.
//!
//! The socket is the whole basis for "same account": it lives in a directory only
//! this account can open, and the journal checks the peer's uid. There is no fallback
//! to the journal's TCP port, which can't be tied to an account here.

use std::future::Future;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use spl_core::pairlink::{self, ParsedPairLink};
use spl_transport::credential::Credential;

use crate::window_control::MarkRenderDocument;

const SOCKET_ENV: &str = "SOLSTONE_JOURNAL_SOCKET";
const IDENTITY_PATH: &str = "/app/link/api/identity";
const PAIR_START_PATH: &str = "/app/network/pair-start";
const PROTOCOL_VERSION_HEADER: &str = "X-Solstone-Protocol-Version";
const PROTOCOL_VERSION: &str = "3";
const READ_TIMEOUT: Duration = Duration::from_secs(2);
const MINT_TIMEOUT: Duration = Duration::from_secs(10);
const PROOF_TIMEOUT: Duration = Duration::from_secs(10);

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the app can ask of the journal on this computer.
pub(crate) trait LocalJournal: Send + Sync {
    /// The instance id of a committed journal under this account, if one answers.
    fn committed_instance(&self) -> BoxFuture<'_, Option<String>>;
    /// A pairing link the journal mints for an app on its own computer.
    fn mint_link<'a>(&'a self, device_label: &'a str) -> BoxFuture<'a, Option<String>>;
    /// Whether the journal behind this credential's pinned, authenticated connection
    /// says it is this committed instance, with that instance's mark.
    fn authenticated_as<'a>(
        &'a self,
        credential: &'a Credential,
        instance_id: &'a str,
    ) -> BoxFuture<'a, bool>;
}

/// The offer the window shows, sent over the app's control bus.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LocalJournalOffer {
    pub instance_id: String,
    pub mark: MarkRenderDocument,
}

pub(crate) fn offer_for(instance_id: String) -> Option<LocalJournalOffer> {
    let mark = crate::mark_cache::render_spec(&instance_id)?.into();
    Some(LocalJournalOffer { instance_id, mark })
}

pub fn encode_offer(offer: Option<&LocalJournalOffer>) -> String {
    serde_json::to_string(&offer).expect("offer serialization")
}

pub fn decode_offer(payload: &str) -> Option<LocalJournalOffer> {
    serde_json::from_str(payload).ok().flatten()
}

/// A same-computer link is direct and names only this computer's IPv4 loopback.
pub(crate) fn link_stays_on_this_computer(link: &str) -> bool {
    let Ok(ParsedPairLink::Direct(direct)) = pairlink::parse(link) else {
        return false;
    };
    !direct.candidates.is_empty()
        && direct.candidates.iter().all(|candidate| {
            candidate
                .host
                .parse::<std::net::Ipv4Addr>()
                .is_ok_and(|ip| ip.is_loopback())
        })
}

#[derive(Deserialize)]
struct IdentityBody {
    #[serde(default)]
    committed: bool,
    #[serde(default)]
    instance_id: Option<String>,
    #[serde(default)]
    mark: Option<serde_json::Value>,
}

/// A committed identity's instance id, when its mark is the one that id derives.
fn committed_instance_in(body: &[u8]) -> Option<String> {
    let identity: IdentityBody = serde_json::from_slice(body).ok()?;
    if !identity.committed {
        return None;
    }
    let instance_id = identity.instance_id?;
    let expected = serde_json::to_value(crate::mark_cache::render_spec(&instance_id)?).ok()?;
    (identity.mark? == expected).then_some(instance_id)
}

/// Keep only what reaches this computer: no LAN address and no relay.
fn on_this_computer_only(credential: &Credential) -> Credential {
    let mut narrowed = credential.clone();
    narrowed.endpoints.retain(|endpoint| {
        endpoint
            .host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
    });
    narrowed.local_endpoints = None;
    narrowed.relay_origin = None;
    narrowed.device_token = None;
    narrowed.device_token_expires_at = None;
    narrowed
}

fn default_socket_path() -> Option<PathBuf> {
    match std::env::var_os(SOCKET_ENV) {
        Some(value) if value.is_empty() => None,
        Some(value) => Some(PathBuf::from(value)),
        None => {
            let home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
            Some(PathBuf::from(home).join(".local/run/solstone/journal.sock"))
        }
    }
}

fn owned_socket(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_socket() && metadata.uid() == rustix::process::geteuid().as_raw()
    })
}

/// The real journal, reached over the owner socket.
#[derive(Default)]
pub(crate) struct SocketLocalJournal {
    socket: Option<PathBuf>,
}

impl SocketLocalJournal {
    #[cfg(test)]
    fn at(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    fn client(&self, timeout: Duration) -> Option<reqwest::Client> {
        let socket = self.socket.clone().or_else(default_socket_path)?;
        if !owned_socket(&socket) {
            return None;
        }
        reqwest::Client::builder()
            .unix_socket(socket)
            .no_proxy()
            // A journal in its first run redirects to setup, which isn't an answer.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .ok()
    }
}

fn socket_url(path: &str) -> String {
    format!("http://localhost{path}")
}

impl LocalJournal for SocketLocalJournal {
    fn committed_instance(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async move {
            let response = self
                .client(READ_TIMEOUT)?
                .get(socket_url(IDENTITY_PATH))
                .send()
                .await
                .ok()?;
            if !response.status().is_success() {
                return None;
            }
            committed_instance_in(&response.bytes().await.ok()?)
        })
    }

    fn mint_link<'a>(&'a self, device_label: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let response = self
                .client(MINT_TIMEOUT)?
                .post(socket_url(PAIR_START_PATH))
                .json(&serde_json::json!({
                    "same_machine": true,
                    "device_label": device_label,
                }))
                .send()
                .await
                .ok()?;
            if !response.status().is_success() {
                return None;
            }
            let body: serde_json::Value = response.json().await.ok()?;
            body.get("pair_link")?.as_str().map(str::to_owned)
        })
    }

    fn authenticated_as<'a>(
        &'a self,
        credential: &'a Credential,
        instance_id: &'a str,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let narrowed = on_this_computer_only(credential);
            if narrowed.endpoints.is_empty() {
                return false;
            }
            let Ok(client) = spl_transport::TransportClient::new(narrowed, None) else {
                return false;
            };
            let headers = [(
                PROTOCOL_VERSION_HEADER.to_owned(),
                PROTOCOL_VERSION.to_owned(),
            )];
            let request = client.request(
                "GET",
                IDENTITY_PATH,
                &headers,
                &[],
                spl_transport::RequestOptions::default(),
            );
            let Ok(Ok(outcome)) = tokio::time::timeout(PROOF_TIMEOUT, request).await else {
                return false;
            };
            outcome.response.status == 200
                && committed_instance_in(&outcome.response.body).as_deref() == Some(instance_id)
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) const LAN_PAIR_LINK: &str =
        "0G0W1A0118EYJ001081G81860W40J2GB1G6GW3X0M6HA7955MTKTHADANEPAVBNF";
    const JID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn jid() -> String {
        crate::journal_mark::sample_credential(JID, "cert")
            .instance_id
            .clone()
    }

    fn identity_body(instance_id: &str) -> String {
        serde_json::json!({
            "committed": true,
            "instance_id": instance_id,
            "mark": crate::mark_cache::render_spec(instance_id),
            "availability": "ready",
        })
        .to_string()
    }

    async fn serve_once(listener: tokio::net::UnixListener, body: String) -> String {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut chunk = [0_u8; 4096];
        while !seen.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            seen.extend_from_slice(&chunk[..read]);
        }
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&seen).into_owned()
    }

    #[tokio::test]
    async fn a_journal_on_this_accounts_socket_is_found_with_its_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let instance = jid();
        let server = tokio::spawn(serve_once(listener, identity_body(&instance)));

        let found = SocketLocalJournal::at(path).committed_instance().await;

        assert_eq!(found, Some(instance));
        assert!(
            server
                .await
                .unwrap()
                .starts_with("GET /app/link/api/identity ")
        );
    }

    #[tokio::test]
    async fn something_at_the_path_that_isnt_a_socket_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sock");
        std::fs::write(&path, b"not a socket").unwrap();
        assert_eq!(
            SocketLocalJournal::at(path).committed_instance().await,
            None
        );
    }

    #[tokio::test]
    async fn no_socket_means_no_journal() {
        let dir = tempfile::tempdir().unwrap();
        let journal = SocketLocalJournal::at(dir.path().join("journal.sock"));
        assert_eq!(journal.committed_instance().await, None);
        assert_eq!(journal.mint_link("this computer").await, None);
    }

    #[test]
    fn a_journal_in_first_run_or_with_someone_elses_mark_is_not_found() {
        let instance = jid();
        assert_eq!(
            committed_instance_in(identity_body(&instance).as_bytes()),
            Some(instance.clone())
        );
        assert_eq!(
            committed_instance_in(br#"{"committed":false,"availability":"none"}"#),
            None
        );
        let mut wrong: serde_json::Value = serde_json::from_str(&identity_body(&instance)).unwrap();
        wrong["mark"]["words"] = serde_json::json!(["not", "yours"]);
        assert_eq!(committed_instance_in(wrong.to_string().as_bytes()), None);
        assert_eq!(committed_instance_in(b"<html>"), None);
    }

    #[test]
    fn only_a_link_to_this_computer_counts() {
        assert!(link_stays_on_this_computer(
            crate::private_link::DIRECT_PAIR_LINK_FOR_TEST
        ));
        assert!(!link_stays_on_this_computer(super::tests::LAN_PAIR_LINK));
        assert!(!link_stays_on_this_computer(
            "https://go.solstone.app/p#nope"
        ));
    }

    #[test]
    fn an_offer_survives_the_bus_and_no_offer_stays_none() {
        let offer = offer_for(jid()).unwrap();
        assert_eq!(decode_offer(&encode_offer(Some(&offer))), Some(offer));
        assert_eq!(decode_offer(&encode_offer(None)), None);
        assert_eq!(decode_offer("{"), None);
    }
}
