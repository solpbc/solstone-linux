// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Startup adoption and keep-both device migration.
//!
//! This owner is the only code allowed to use a credential while a copied or
//! changed machine marker is unresolved. Its journal requests are deliberately
//! isolated behind `MigrationTransport`; the regular linked owner and uploader
//! are not admitted until this state machine returns a credential.

use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    time::Duration,
};

use rcgen::{CertificateParams, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spl_transport::{
    RequestOptions, TransportClient,
    credential::{Credential, EndpointAddr},
    request::ReplayPolicy,
};

const STATE_FILENAME: &str = "device-migration.json";
const MARKER_DOMAIN: &[u8] = b"solstone-linux/device-migration/machine-id/v1\0";
const MIGRATION_PATH: &str = "/app/network/api/clients/self/migration";
const REKEY_PATH: &str = "/app/network/api/clients/self/rekey";
const RESPONSE_LIMIT: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
// Hostnames stay out of persisted adoption metadata and migration requests.
const DEVICE_LABEL: &str = "solstone-linux";

pub(crate) type RequestFuture<'a> =
    Pin<Box<dyn Future<Output = Result<MigrationResponse, ()>> + Send + 'a>>;
type StoreFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ()>> + Send + 'a>>;
pub(crate) type StartupResolveFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Credential, String>> + Send + 'a>>;

pub(crate) trait StartupResolver: Send + Sync {
    fn resolve<'a>(&'a self, root: &'a Path, credential: Credential) -> StartupResolveFuture<'a>;
}

pub(crate) struct SystemStartupResolver;

impl StartupResolver for SystemStartupResolver {
    fn resolve<'a>(&'a self, root: &'a Path, credential: Credential) -> StartupResolveFuture<'a> {
        Box::pin(resolve_at_startup(root, credential))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdoptionState {
    version: u8,
    #[serde(default)]
    adopted_marker_digest: Option<String>,
    #[serde(default)]
    pending: Option<PendingMigration>,
    #[serde(default)]
    setup_pairing_id: Option<String>,
}

impl Default for AdoptionState {
    fn default() -> Self {
        Self {
            version: 1,
            adopted_marker_digest: None,
            pending: None,
            setup_pairing_id: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PendingMigration {
    marker_digest: String,
    previous_marker_digest: Option<String>,
    old_credential: Credential,
    old_cid: String,
    old_pairing_id: String,
    transfer_confirmed: bool,
    operation_id: String,
    request_bytes: Vec<u8>,
    candidate_key_pem: String,
    csr_pem: String,
    rekey_response_bytes: Option<Vec<u8>>,
    new_credential: Option<Credential>,
    candidate_cid: Option<String>,
    decision_id: Option<String>,
    decision_bytes: Option<Vec<u8>>,
    decision_response_bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MachineMarker {
    Present(String),
    Missing,
}

pub(crate) trait MarkerProvider: Send + Sync {
    fn read_marker(&self) -> Result<MachineMarker, ()>;
}

pub(crate) trait MigrationClock: Send + Sync {
    fn unix_seconds(&self) -> i64;
}

pub(crate) trait OperationIdProvider: Send + Sync {
    fn next_uuid(&self) -> Result<String, ()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MigrationResponse {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

pub(crate) trait MigrationTransport: Send + Sync {
    fn request<'a>(
        &'a self,
        credential: &'a Credential,
        method: &'a str,
        path: &'a str,
        body: &'a [u8],
    ) -> RequestFuture<'a>;
}

pub(crate) trait MigrationStore: Send + Sync {
    fn load_state(&self) -> Result<Option<AdoptionState>, ()>;
    fn persist_state(&self, state: &AdoptionState) -> Result<(), ()>;
    fn read_answer(&self) -> Result<Option<String>, ()>;
    fn adopt_initial<'a>(&'a self, marker_digest: &'a str, pairing_id: &'a str) -> StoreFuture<'a>;
    fn persist_credential(&self, credential: &Credential) -> Result<(), ()>;
    fn transfer_answer<'a>(
        &'a self,
        old_pairing_id: &'a str,
        new_pairing_id: &'a str,
        previously_confirmed: bool,
    ) -> StoreFuture<'a>;
}

pub(crate) struct SystemMarkerProvider;

impl MarkerProvider for SystemMarkerProvider {
    fn read_marker(&self) -> Result<MachineMarker, ()> {
        match std::fs::read_to_string("/etc/machine-id") {
            Ok(value) => {
                let marker = value.trim();
                if marker.is_empty() {
                    Err(())
                } else {
                    Ok(MachineMarker::Present(marker.to_owned()))
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(MachineMarker::Missing),
            Err(_) => Err(()),
        }
    }
}

pub(crate) struct SystemMigrationClock;

impl MigrationClock for SystemMigrationClock {
    fn unix_seconds(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}

pub(crate) struct SystemOperationIdProvider;

impl OperationIdProvider for SystemOperationIdProvider {
    fn next_uuid(&self) -> Result<String, ()> {
        let value = std::fs::read_to_string("/proc/sys/kernel/random/uuid").map_err(|_| ())?;
        let value = value.trim();
        is_uuid(value).then(|| value.to_owned()).ok_or(())
    }
}

pub(crate) struct FileMigrationStore {
    root: PathBuf,
}

impl FileMigrationStore {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }
}

impl MigrationStore for FileMigrationStore {
    fn load_state(&self) -> Result<Option<AdoptionState>, ()> {
        let path = self.root.join(STATE_FILENAME);
        let bytes = crate::private_link::read_private_file(
            &path,
            crate::private_link::PrivateTargetKind::Migration,
        )
        .map_err(|_| ())?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let state: AdoptionState = serde_json::from_slice(&bytes).map_err(|_| ())?;
        (state.version == 1).then_some(Some(state)).ok_or(())
    }

    fn persist_state(&self, state: &AdoptionState) -> Result<(), ()> {
        let bytes = serde_json::to_vec(state).map_err(|_| ())?;
        crate::private_file::atomic_write_bytes(&self.root.join(STATE_FILENAME), &bytes)
            .map_err(|_| ())
    }

    fn read_answer(&self) -> Result<Option<String>, ()> {
        crate::journal_mark::read_pairing_answer(&self.root)
            .map(|answer| answer.map(|answer| answer.confirmed))
            .map_err(|_| ())
    }

    fn adopt_initial<'a>(&'a self, marker_digest: &'a str, pairing_id: &'a str) -> StoreFuture<'a> {
        Box::pin(async move {
            let _answer_lock = crate::journal_mark::AnswerLock::acquire(&self.root)
                .await
                .map_err(|_| ())?;
            let answer = self.read_answer()?;
            if answer.as_deref().is_some_and(|value| value != pairing_id) {
                return Err(());
            }
            let mut state = self.load_state()?.unwrap_or_default();
            if state.adopted_marker_digest.is_some() || state.pending.is_some() {
                return Err(());
            }
            state.adopted_marker_digest = Some(marker_digest.to_owned());
            self.persist_state(&state)
        })
    }

    fn persist_credential(&self, credential: &Credential) -> Result<(), ()> {
        crate::private_link::persist_credential(&self.root, credential).map_err(|_| ())
    }

    fn transfer_answer<'a>(
        &'a self,
        old_pairing_id: &'a str,
        new_pairing_id: &'a str,
        previously_confirmed: bool,
    ) -> StoreFuture<'a> {
        Box::pin(async move {
            let _answer_lock = crate::journal_mark::AnswerLock::acquire(&self.root)
                .await
                .map_err(|_| ())?;
            let current = self.read_answer()?;
            match current.as_deref() {
                Some(value) if value == new_pairing_id => Ok(()),
                Some(value) if value == old_pairing_id && previously_confirmed => {
                    crate::journal_mark::write_pairing_answer(&self.root, new_pairing_id)
                        .map_err(|_| ())
                }
                None if !previously_confirmed => Ok(()),
                _ => Err(()),
            }
        })
    }
}

pub(crate) struct SplMigrationTransport;

impl MigrationTransport for SplMigrationTransport {
    fn request<'a>(
        &'a self,
        credential: &'a Credential,
        method: &'a str,
        path: &'a str,
        body: &'a [u8],
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let client = if credential.endpoints.is_empty()
                && credential.relay_origin.is_some()
                && credential.device_token.is_some()
            {
                TransportClient::new_relay_only(credential.clone(), None)
            } else {
                TransportClient::new(credential.clone(), None)
            }
            .map_err(|_| ())?;
            let headers = if body.is_empty() {
                Vec::new()
            } else {
                vec![("content-type".to_owned(), "application/json".to_owned())]
            };
            let outcome = tokio::time::timeout(
                REQUEST_TIMEOUT,
                client.request(
                    method,
                    path,
                    &headers,
                    body,
                    RequestOptions {
                        response_cap: RESPONSE_LIMIT,
                        replay: ReplayPolicy::ReplaySafe,
                        observer: None,
                    },
                ),
            )
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?;
            Ok(MigrationResponse {
                status: outcome.response.status,
                body: outcome.response.body,
            })
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MigrationError {
    Marker,
    State,
    Answer,
    Identity,
    Protocol,
    Transport,
    Persistence,
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Marker => "machine marker unavailable",
            Self::State => "migration state unavailable",
            Self::Answer => "journal mark is not confirmed",
            Self::Identity => "migration identity validation failed",
            Self::Protocol => "device migration protocol rejected",
            Self::Transport => "device migration transport unavailable",
            Self::Persistence => "migration state could not be persisted",
        })
    }
}

pub(crate) async fn resolve_at_startup(
    root: &Path,
    credential: Credential,
) -> Result<Credential, String> {
    resolve(
        &FileMigrationStore::new(root),
        &SystemMarkerProvider,
        &SystemOperationIdProvider,
        &SystemMigrationClock,
        &SplMigrationTransport,
        credential,
    )
    .await
    .map_err(|error| error.to_string())
}

pub(crate) fn prepare_setup_pairing_at(root: &Path, pairing_id: &str) -> Result<(), ()> {
    prepare_setup_pairing(&FileMigrationStore::new(root), pairing_id)
}

pub(crate) fn should_grandfather_setup_answer_at(root: &Path) -> Result<bool, ()> {
    let Some(state) = FileMigrationStore::new(root).load_state()? else {
        return Ok(true);
    };
    Ok(!state
        .pending
        .as_ref()
        .is_some_and(|pending| !pending.transfer_confirmed))
}

pub(crate) fn finish_setup_pairing_at(
    root: &Path,
    marker_provider: &dyn MarkerProvider,
    pairing_id: &str,
) -> Result<(), ()> {
    finish_setup_pairing(&FileMigrationStore::new(root), marker_provider, pairing_id)
}

fn prepare_setup_pairing(store: &dyn MigrationStore, pairing_id: &str) -> Result<(), ()> {
    let Some(mut state) = store.load_state()? else {
        return Ok(());
    };
    if state.pending.is_none() {
        return Ok(());
    }
    state.setup_pairing_id = Some(pairing_id.to_owned());
    store.persist_state(&state)
}

fn finish_setup_pairing(
    store: &dyn MigrationStore,
    marker_provider: &dyn MarkerProvider,
    pairing_id: &str,
) -> Result<(), ()> {
    let Some(mut state) = store.load_state()? else {
        return Ok(());
    };
    if state.setup_pairing_id.is_none() && state.pending.is_none() {
        return Ok(());
    }
    if state.setup_pairing_id.as_deref() != Some(pairing_id)
        || store.read_answer()?.as_deref() != Some(pairing_id)
    {
        return Err(());
    }
    let marker = marker_provider.read_marker()?;
    state.pending = None;
    state.setup_pairing_id = None;
    state.adopted_marker_digest = Some(marker_digest(marker));
    store.persist_state(&state)
}

fn persist(store: &dyn MigrationStore, state: &AdoptionState) -> Result<(), MigrationError> {
    store
        .persist_state(state)
        .map_err(|_| MigrationError::Persistence)
}

async fn resolve(
    store: &dyn MigrationStore,
    marker_provider: &dyn MarkerProvider,
    ids: &dyn OperationIdProvider,
    clock: &dyn MigrationClock,
    transport: &dyn MigrationTransport,
    credential: Credential,
) -> Result<Credential, MigrationError> {
    let marker_digest = marker_digest(
        marker_provider
            .read_marker()
            .map_err(|_| MigrationError::Marker)?,
    );
    let mut state = store
        .load_state()
        .map_err(|_| MigrationError::State)?
        .unwrap_or_default();
    if state.version != 1 {
        return Err(MigrationError::State);
    }

    if let Some(setup_pairing_id) = state.setup_pairing_id.as_deref() {
        let current_pairing_id =
            crate::private_link::compute_pairing_id(&credential.client_cert_pem);
        if current_pairing_id == setup_pairing_id {
            if store
                .read_answer()
                .map_err(|_| MigrationError::Answer)?
                .as_deref()
                != Some(setup_pairing_id)
            {
                return Err(MigrationError::Answer);
            }
            state.adopted_marker_digest = Some(marker_digest);
            state.pending = None;
            state.setup_pairing_id = None;
            persist(store, &state)?;
            return Ok(credential);
        }
        state.setup_pairing_id = None;
        persist(store, &state)?;
    }

    if let Some(pending) = state.pending.as_mut() {
        if !credential_identity_matches(&credential, &pending.old_credential)
            && !pending
                .new_credential
                .as_ref()
                .is_some_and(|candidate| credential_identity_matches(&credential, candidate))
        {
            return Err(MigrationError::Identity);
        }
        if pending.marker_digest != marker_digest {
            // A move copied from another machine is never resumed here; this
            // machine starts its own with a fresh key and operation. The
            // journal does not refuse a new rekey while the copied one stays
            // pending, so the copied key is never used.
            state.pending = None;
            persist(store, &state)?;
        } else {
            let resolved = resume_pending(store, ids, clock, transport, pending).await?;
            state.adopted_marker_digest = Some(marker_digest);
            state.pending = None;
            persist(store, &state)?;
            return Ok(resolved);
        }
    }

    if state.adopted_marker_digest.as_deref() == Some(marker_digest.as_str()) {
        if credential_answer_is_accepted(store, &credential)? {
            return Ok(credential);
        }
        return Err(MigrationError::Answer);
    }

    let old_pairing_id = crate::private_link::compute_pairing_id(&credential.client_cert_pem);
    if state.adopted_marker_digest.is_none() {
        // The store holds the answer lock only for this local publication; it
        // neither manufactures an answer for legacy absence nor holds a lock
        // during a migration request.
        store
            .adopt_initial(&marker_digest, &old_pairing_id)
            .await
            .map_err(|_| MigrationError::Answer)?;
        return Ok(credential);
    }

    let transfer_confirmed = match store
        .read_answer()
        .map_err(|_| MigrationError::Answer)?
        .as_deref()
    {
        None => false, // Preserve legacy absent-answer grandfathering as absence.
        Some(value) if value == old_pairing_id => true,
        Some(_) => return Err(MigrationError::Answer),
    };

    let old_cid = credential_cid(&credential)?;
    let pending = create_pending(
        marker_digest.clone(),
        state.adopted_marker_digest.clone(),
        credential,
        old_cid,
        old_pairing_id,
        transfer_confirmed,
        ids,
    )?;
    state.pending = Some(pending);
    persist(store, &state)?;
    let pending = state.pending.as_mut().ok_or(MigrationError::State)?;
    let resolved = resume_pending(store, ids, clock, transport, pending).await?;
    state.adopted_marker_digest = Some(marker_digest);
    state.pending = None;
    persist(store, &state)?;
    Ok(resolved)
}

fn credential_answer_is_accepted(
    store: &dyn MigrationStore,
    credential: &Credential,
) -> Result<bool, MigrationError> {
    let answer = store.read_answer().map_err(|_| MigrationError::Answer)?;
    let pairing_id = crate::private_link::compute_pairing_id(&credential.client_cert_pem);
    Ok(answer.as_deref().is_none_or(|value| value == pairing_id))
}

fn create_pending(
    marker_digest: String,
    previous_marker_digest: Option<String>,
    old_credential: Credential,
    old_cid: String,
    old_pairing_id: String,
    transfer_confirmed: bool,
    ids: &dyn OperationIdProvider,
) -> Result<PendingMigration, MigrationError> {
    let key =
        KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|_| MigrationError::Identity)?;
    let mut params =
        CertificateParams::new(Vec::<String>::new()).map_err(|_| MigrationError::Identity)?;
    params
        .distinguished_name
        .push(DnType::CommonName, DEVICE_LABEL);
    let csr_pem = params
        .serialize_request(&key)
        .and_then(|csr| csr.pem())
        .map_err(|_| MigrationError::Identity)?;
    let operation_id = ids.next_uuid().map_err(|_| MigrationError::Identity)?;
    if !is_uuid(&operation_id) {
        return Err(MigrationError::Identity);
    }
    let request = json!({
        "protocol_version":1,
        "operation_id":operation_id,
        "csr":csr_pem,
        "device_label":DEVICE_LABEL,
        "client_label":DEVICE_LABEL,
        "platform":"linux"
    });
    let request_bytes = serde_json::to_vec(&request).map_err(|_| MigrationError::Protocol)?;
    Ok(PendingMigration {
        marker_digest,
        previous_marker_digest,
        old_credential,
        old_cid,
        old_pairing_id,
        transfer_confirmed,
        operation_id,
        request_bytes,
        candidate_key_pem: key.serialize_pem(),
        csr_pem,
        rekey_response_bytes: None,
        new_credential: None,
        candidate_cid: None,
        decision_id: None,
        decision_bytes: None,
        decision_response_bytes: None,
    })
}

async fn resume_pending(
    store: &dyn MigrationStore,
    ids: &dyn OperationIdProvider,
    clock: &dyn MigrationClock,
    transport: &dyn MigrationTransport,
    pending: &mut PendingMigration,
) -> Result<Credential, MigrationError> {
    validate_saved_request(pending)?;
    if credential_cid(&pending.old_credential)? != pending.old_cid
        || crate::private_link::compute_pairing_id(&pending.old_credential.client_cert_pem)
            != pending.old_pairing_id
        || !is_uuid(&pending.operation_id)
    {
        return Err(MigrationError::Identity);
    }
    match pending.rekey_response_bytes.as_deref() {
        Some(bytes) => {
            let response = parse_rekey(bytes, pending)?;
            let reconstructed = credential_from_pairing(
                &pending.old_credential,
                &pending.candidate_key_pem,
                response,
                clock.unix_seconds(),
            )?;
            if pending.new_credential.as_ref() != Some(&reconstructed)
                || pending.candidate_cid.as_deref()
                    != Some(credential_cid(&reconstructed)?.as_str())
            {
                return Err(MigrationError::Identity);
            }
        }
        None if pending.new_credential.is_some()
            || pending.candidate_cid.is_some()
            || pending.decision_id.is_some()
            || pending.decision_bytes.is_some()
            || pending.decision_response_bytes.is_some() =>
        {
            return Err(MigrationError::State);
        }
        None => {}
    }
    if pending.decision_response_bytes.is_none() {
        if pending.rekey_response_bytes.is_none() {
            let state = get_migration_state(transport, &pending.old_credential).await?;
            match state.state {
                ProtocolState::None | ProtocolState::NewDevice | ProtocolState::SameDevice => {}
                ProtocolState::Pending
                    if state.rekey_operation_id.as_deref() == Some(&pending.operation_id)
                        && state.previous_cid.as_deref() == Some(&pending.old_cid) => {}
                _ => return Err(MigrationError::Protocol),
            }
            let response = transport
                .request(
                    &pending.old_credential,
                    "POST",
                    REKEY_PATH,
                    &pending.request_bytes,
                )
                .await
                .map_err(|_| MigrationError::Transport)?;
            if response.status != 200 && response.status != 201 {
                return Err(MigrationError::Protocol);
            }
            let rekey = parse_rekey(&response.body, pending)?;
            let candidate = credential_from_pairing(
                &pending.old_credential,
                &pending.candidate_key_pem,
                rekey,
                clock.unix_seconds(),
            )?;
            pending.candidate_cid = Some(credential_cid(&candidate)?);
            pending.new_credential = Some(candidate);
            pending.rekey_response_bytes = Some(response.body);
            persist(store, &state_from_pending(pending))?;
        }
        if pending.decision_id.is_none() {
            let decision_id = ids.next_uuid().map_err(|_| MigrationError::Identity)?;
            if !is_uuid(&decision_id) {
                return Err(MigrationError::Identity);
            }
            let request = json!({
                "protocol_version":1,
                "operation_id":decision_id,
                "choice":"new_device"
            });
            pending.decision_bytes =
                Some(serde_json::to_vec(&request).map_err(|_| MigrationError::Protocol)?);
            pending.decision_id = Some(decision_id);
            persist(store, &state_from_pending(pending))?;
        }
        let candidate = pending
            .new_credential
            .as_ref()
            .ok_or(MigrationError::State)?;
        let migration_state = get_migration_state(transport, candidate).await?;
        if migration_state.state == ProtocolState::NewDevice
            && migration_state.rekey_operation_id.as_deref() == Some(&pending.operation_id)
            && migration_state.previous_cid.as_deref() == Some(&pending.old_cid)
        {
            pending.decision_response_bytes = Some(
                serde_json::to_vec(&json!({
                    "protocol_version":1,
                    "operation_id":pending.decision_id,
                    "state":"new_device",
                    "previous_cid":pending.old_cid,
                    "cid":pending.candidate_cid,
                    "replaced_cid":null,
                    "display_label":candidate.home_label
                }))
                .map_err(|_| MigrationError::Protocol)?,
            );
            persist(store, &state_from_pending(pending))?;
        } else if migration_state.state == ProtocolState::Pending
            && migration_state.rekey_operation_id.as_deref() == Some(&pending.operation_id)
            && migration_state.previous_cid.as_deref() == Some(&pending.old_cid)
        {
            let response = transport
                .request(
                    candidate,
                    "PUT",
                    MIGRATION_PATH,
                    pending
                        .decision_bytes
                        .as_deref()
                        .ok_or(MigrationError::State)?,
                )
                .await
                .map_err(|_| MigrationError::Transport)?;
            if response.status != 200 && response.status != 201 {
                return Err(MigrationError::Protocol);
            }
            validate_decision(&response.body, pending)?;
            pending.decision_response_bytes = Some(response.body);
            persist(store, &state_from_pending(pending))?;
        } else {
            return Err(MigrationError::Protocol);
        }
    }

    let new_credential = pending
        .new_credential
        .as_ref()
        .ok_or(MigrationError::State)?;
    let new_pairing_id = crate::private_link::compute_pairing_id(&new_credential.client_cert_pem);
    if let Some(response) = pending.decision_response_bytes.as_deref() {
        validate_decision(response, pending)?;
    } else {
        return Err(MigrationError::Protocol);
    }
    if pending.candidate_cid.as_deref() != Some(credential_cid(new_credential)?.as_str()) {
        return Err(MigrationError::Identity);
    }
    // Re-publishing is intentional: it recovers a crash after credential rename
    // but before the migration checkpoint reached durable storage.
    store
        .persist_credential(new_credential)
        .map_err(|_| MigrationError::Persistence)?;
    store
        .transfer_answer(
            &pending.old_pairing_id,
            &new_pairing_id,
            pending.transfer_confirmed,
        )
        .await
        .map_err(|_| MigrationError::Answer)?;
    Ok(new_credential.clone())
}

fn state_from_pending(pending: &PendingMigration) -> AdoptionState {
    AdoptionState {
        version: 1,
        adopted_marker_digest: pending.previous_marker_digest.clone(),
        pending: Some(pending.clone()),
        setup_pairing_id: None,
    }
}

async fn get_migration_state(
    transport: &dyn MigrationTransport,
    credential: &Credential,
) -> Result<MigrationStateResponse, MigrationError> {
    let response = transport
        .request(credential, "GET", MIGRATION_PATH, &[])
        .await
        .map_err(|_| MigrationError::Transport)?;
    if response.status != 200 {
        return Err(MigrationError::Transport);
    }
    parse_migration_state(&response.body).map_err(|_| MigrationError::Protocol)
}

fn parse_migration_state(bytes: &[u8]) -> Result<MigrationStateResponse, ()> {
    let parsed: MigrationStateResponse = parse_closed(bytes)?;
    let operation_id = parsed.rekey_operation_id.as_deref();
    let previous_cid = parsed.previous_cid.as_deref();
    let replaced_cid = parsed.replaced_cid.as_deref();
    let valid = parsed.protocol_version == 1
        && parsed.rekey_operation_id.present
        && parsed.previous_cid.present
        && parsed.replaced_cid.present
        && operation_id.is_none_or(is_uuid)
        && previous_cid.is_none_or(is_cid)
        && replaced_cid.is_none_or(is_cid)
        && match parsed.state {
            ProtocolState::None => {
                operation_id.is_none() && previous_cid.is_none() && replaced_cid.is_none()
            }
            ProtocolState::Pending | ProtocolState::NewDevice => {
                operation_id.is_some() && previous_cid.is_some() && replaced_cid.is_none()
            }
            ProtocolState::SameDevice => {
                operation_id.is_some() && previous_cid.is_some() && replaced_cid == previous_cid
            }
            ProtocolState::ReplacedDevice => {
                operation_id.is_none() && previous_cid.is_none() && replaced_cid.is_some()
            }
        };
    valid.then_some(parsed).ok_or(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RekeyResponse {
    protocol_version: u8,
    operation_id: String,
    state: ProtocolState,
    previous_cid: String,
    cid: String,
    pairing: PairingResponse,
}

// The contract deliberately leaves pairing additive. Known credential fields
// are required below, while future pairing metadata remains tolerated.
#[derive(Deserialize)]
struct PairingResponse {
    client_cert: String,
    ca_chain: Vec<String>,
    instance_id: String,
    home_label: String,
    fingerprint: String,
    #[serde(default)]
    home_attestation: Option<String>,
    #[serde(default)]
    local_endpoints: Option<Value>,
    #[serde(default)]
    relay_access: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrationStateResponse {
    protocol_version: u8,
    #[serde(default)]
    rekey_operation_id: RequiredNullableString,
    #[serde(default)]
    previous_cid: RequiredNullableString,
    state: ProtocolState,
    #[serde(default)]
    replaced_cid: RequiredNullableString,
}

#[derive(Default)]
struct RequiredNullableString {
    present: bool,
    value: Option<String>,
}

impl RequiredNullableString {
    fn as_deref(&self) -> Option<&str> {
        self.value.as_deref()
    }
}

impl<'de> Deserialize<'de> for RequiredNullableString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer).map(|value| Self {
            present: true,
            value,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionResponse {
    protocol_version: u8,
    operation_id: String,
    state: ProtocolState,
    #[serde(default)]
    previous_cid: RequiredNullableString,
    cid: String,
    #[serde(default)]
    replaced_cid: RequiredNullableString,
    #[serde(rename = "display_label")]
    _display_label: String,
}

fn parse_closed<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ()> {
    serde_json::from_slice(bytes).map_err(|_| ())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RekeyRequest {
    protocol_version: u8,
    operation_id: String,
    csr: String,
    device_label: String,
    client_label: String,
    platform: ProtocolPlatform,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProtocolPlatform {
    Linux,
    Macos,
    Windows,
    Ios,
    Android,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionRequest {
    protocol_version: u8,
    operation_id: String,
    choice: DecisionChoice,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
enum DecisionChoice {
    #[serde(rename = "new_device")]
    New,
    #[serde(rename = "same_device")]
    Same,
    #[serde(rename = "replace_device")]
    Replace,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProtocolState {
    None,
    Pending,
    NewDevice,
    SameDevice,
    ReplacedDevice,
}

fn validate_saved_request(pending: &PendingMigration) -> Result<(), MigrationError> {
    let request: RekeyRequest =
        parse_closed(&pending.request_bytes).map_err(|_| MigrationError::Protocol)?;
    let key =
        KeyPair::from_pem(&pending.candidate_key_pem).map_err(|_| MigrationError::Identity)?;
    if request.protocol_version != 1
        || request.operation_id != pending.operation_id
        || request.csr != pending.csr_pem
        || request.device_label != DEVICE_LABEL
        || request.client_label != DEVICE_LABEL
        || request.platform != ProtocolPlatform::Linux
        || key.algorithm() != &PKCS_ECDSA_P256_SHA256
    {
        return Err(MigrationError::Identity);
    }
    match (
        pending.decision_id.as_deref(),
        pending.decision_bytes.as_deref(),
    ) {
        (Some(id), Some(bytes)) => {
            let request: DecisionRequest =
                parse_closed(bytes).map_err(|_| MigrationError::Protocol)?;
            if !is_uuid(id)
                || request.protocol_version != 1
                || request.operation_id != id
                || request.choice != DecisionChoice::New
            {
                return Err(MigrationError::Protocol);
            }
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(MigrationError::State),
    }
}

fn parse_rekey(bytes: &[u8], pending: &PendingMigration) -> Result<RekeyResponse, MigrationError> {
    let response: RekeyResponse = parse_closed(bytes).map_err(|_| MigrationError::Protocol)?;
    if response.protocol_version != 1
        || response.operation_id != pending.operation_id
        || response.state != ProtocolState::Pending
        || response.previous_cid != pending.old_cid
        || !is_cid(&response.previous_cid)
        || !is_cid(&response.cid)
        || response.pairing.fingerprint != response.cid
    {
        return Err(MigrationError::Protocol);
    }
    Ok(response)
}

fn validate_decision(bytes: &[u8], pending: &PendingMigration) -> Result<(), MigrationError> {
    let response: DecisionResponse = parse_closed(bytes).map_err(|_| MigrationError::Protocol)?;
    let candidate_cid = pending
        .candidate_cid
        .as_deref()
        .ok_or(MigrationError::State)?;
    if response.protocol_version != 1
        || response.operation_id != pending.decision_id.as_deref().unwrap_or_default()
        || response.state != ProtocolState::NewDevice
        || !response.previous_cid.present
        || !response.replaced_cid.present
        || response.previous_cid.as_deref() != Some(&pending.old_cid)
        || response.cid != candidate_cid
        || response.replaced_cid.as_deref().is_some()
    {
        return Err(MigrationError::Protocol);
    }
    Ok(())
}

fn credential_from_pairing(
    old: &Credential,
    key_pem: &str,
    response: RekeyResponse,
    now: i64,
) -> Result<Credential, MigrationError> {
    let pairing = response.pairing;
    if pairing.instance_id != old.instance_id {
        return Err(MigrationError::Identity);
    }
    let certs = spl_transport::tls::parse_certs(&pairing.client_cert)
        .map_err(|_| MigrationError::Identity)?;
    let leaf = certs.first().ok_or(MigrationError::Identity)?;
    let computed_cid = format!("sha256:{}", spl_core::ca::sha256_hex(leaf.as_ref()));
    if computed_cid != response.cid {
        return Err(MigrationError::Identity);
    }
    let key = KeyPair::from_pem(key_pem).map_err(|_| MigrationError::Identity)?;
    if key.algorithm() != &PKCS_ECDSA_P256_SHA256
        || spl_core::ca::extract_spki_der(leaf.as_ref()).map_err(|_| MigrationError::Identity)?
            != key.public_key_der()
    {
        return Err(MigrationError::Identity);
    }
    let ca_chain_pem = pairing.ca_chain;
    let ca_certs = ca_chain_pem
        .iter()
        .map(|pem| spl_transport::tls::parse_certs(pem).map_err(|_| MigrationError::Identity))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let pinned_ca: Vec<_> = ca_certs
        .iter()
        .filter(|cert| spl_core::ca::cert_matches_prefix(cert.as_ref(), &old.ca_fp_prefix))
        .collect();
    if pinned_ca.len() != 1 {
        return Err(MigrationError::Identity);
    }
    let ca_spki = spl_core::ca::extract_spki_der(pinned_ca[0].as_ref())
        .map_err(|_| MigrationError::Identity)?;
    let ca_jid =
        spl_core::relay_window::jid_from_spki(&ca_spki).map_err(|_| MigrationError::Identity)?;
    if ca_jid != old.instance_id {
        return Err(MigrationError::Identity);
    }
    let endpoints = match pairing.local_endpoints.as_ref() {
        Some(value) => endpoints_from_local(Some(value)).ok_or(MigrationError::Identity)?,
        None => old.endpoints.clone(),
    };
    let mut relay_origin = None;
    let mut device_token = None;
    let mut device_token_expires_at = None;
    if let Some(access_value) = pairing.relay_access {
        let access: spl_core::relay_access::RelayAccess =
            serde_json::from_value(access_value).map_err(|_| MigrationError::Identity)?;
        let claims =
            relay_access_claims(&access, &old.instance_id, now).ok_or(MigrationError::Identity)?;
        relay_origin = Some(access.relay_origin);
        device_token = Some(access.device_token);
        device_token_expires_at = Some(claims.exp);
    }
    let credential = Credential {
        client_key_pem: key_pem.to_owned(),
        client_cert_pem: pairing.client_cert,
        ca_chain_pem,
        ca_fp_prefix: old.ca_fp_prefix.clone(),
        instance_id: pairing.instance_id,
        home_label: pairing.home_label,
        endpoints,
        home_attestation: pairing.home_attestation,
        local_endpoints: pairing.local_endpoints,
        relay_origin,
        device_token,
        device_token_expires_at,
    };
    if credential.endpoints.is_empty() {
        if credential.relay_origin.is_some() && credential.device_token.is_none() {
            return Err(MigrationError::Identity);
        }
        if credential.relay_origin.is_some() {
            TransportClient::new_relay_only(credential.clone(), None)
                .map_err(|_| MigrationError::Identity)?;
        } else {
            return Err(MigrationError::Identity);
        }
    } else {
        TransportClient::new(credential.clone(), None).map_err(|_| MigrationError::Identity)?;
    }
    Ok(credential)
}

// The journal replays the relay access it issued with the rekey, and a saved
// rekey reply is resumed later, so the token may have expired. Its shape and
// home binding are still checked; relay renewal refreshes an expired token.
fn relay_access_claims(
    access: &spl_core::relay_access::RelayAccess,
    instance_id: &str,
    now: i64,
) -> Option<spl_core::jwt::JwtClaims> {
    let expires = spl_core::relay_access::unverified_payload(&access.device_token)?
        .get("exp")?
        .as_i64()?;
    access.claims(instance_id, now.min(expires.checked_sub(1)?))
}

fn endpoints_from_local(value: Option<&Value>) -> Option<Vec<EndpointAddr>> {
    let items = value?.as_array()?;
    let mut endpoints = Vec::with_capacity(items.len());
    for item in items {
        let host = item.get("ip")?.as_str()?;
        let port = u16::try_from(item.get("port")?.as_u64()?).ok()?;
        if host.is_empty() || port == 0 {
            return None;
        }
        endpoints.push(EndpointAddr {
            host: host.to_owned(),
            port,
        });
    }
    Some(endpoints)
}

fn credential_cid(credential: &Credential) -> Result<String, MigrationError> {
    let certs = spl_transport::tls::parse_certs(&credential.client_cert_pem)
        .map_err(|_| MigrationError::Identity)?;
    let leaf = certs.first().ok_or(MigrationError::Identity)?;
    Ok(format!(
        "sha256:{}",
        spl_core::ca::sha256_hex(leaf.as_ref())
    ))
}

fn credential_identity_matches(current: &Credential, expected: &Credential) -> bool {
    current.client_cert_pem == expected.client_cert_pem
        && current.client_key_pem == expected.client_key_pem
        && current.instance_id == expected.instance_id
}

fn marker_digest(marker: MachineMarker) -> String {
    let mut digest = Sha256::new();
    digest.update(MARKER_DOMAIN);
    match marker {
        MachineMarker::Present(marker) => {
            digest.update(b"present\0");
            digest.update((marker.len() as u64).to_be_bytes());
            digest.update(marker.as_bytes());
        }
        MachineMarker::Missing => digest.update(b"missing\0"),
    }
    format!("{:x}", digest.finalize())
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|index| bytes[*index] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
}

fn is_cid(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(crate) fn owner_identity_action_allowed(
    root: &Path,
    marker_provider: &dyn MarkerProvider,
) -> bool {
    match FileMigrationStore::new(root).load_state() {
        Ok(None) => true,
        Ok(Some(state)) => state_admits_credential(&state, marker_provider),
        Err(()) => false,
    }
}

pub(crate) fn credential_admission_allowed(
    root: &Path,
    marker_provider: &dyn MarkerProvider,
) -> bool {
    FileMigrationStore::new(root)
        .load_state()
        .is_ok_and(|state| {
            state.is_some_and(|state| state_admits_credential(&state, marker_provider))
        })
}

fn state_admits_credential(state: &AdoptionState, marker_provider: &dyn MarkerProvider) -> bool {
    let Ok(marker) = marker_provider.read_marker() else {
        return false;
    };
    state.pending.is_none()
        && state.adopted_marker_digest.as_deref() == Some(marker_digest(marker).as_str())
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use rcgen::{
        BasicConstraints, Certificate, CertificateParams, CertificateSigningRequestParams,
        ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
    };
    use spl_transport::credential::EndpointAddr;

    const OLD_ID: &str = "123e4567-e89b-42d3-a456-426614174000";
    const COPIED_DECISION_ID: &str = "123e4567-e89b-42d3-a456-426614174001";
    const NEW_ID: &str = "123e4567-e89b-42d3-a456-426614174002";
    const NEW_DECISION_ID: &str = "123e4567-e89b-42d3-a456-426614174003";

    struct TestMarker(MachineMarker);

    impl MarkerProvider for TestMarker {
        fn read_marker(&self) -> Result<MachineMarker, ()> {
            Ok(self.0.clone())
        }
    }

    struct UnavailableMarker;

    impl MarkerProvider for UnavailableMarker {
        fn read_marker(&self) -> Result<MachineMarker, ()> {
            Err(())
        }
    }

    struct TestClock;

    impl MigrationClock for TestClock {
        fn unix_seconds(&self) -> i64 {
            1_800_000_000
        }
    }

    struct TestIds(Mutex<VecDeque<String>>);

    impl TestIds {
        fn new(ids: &[&str]) -> Self {
            Self(Mutex::new(ids.iter().map(|id| (*id).to_owned()).collect()))
        }
    }

    impl OperationIdProvider for TestIds {
        fn next_uuid(&self) -> Result<String, ()> {
            self.0.lock().unwrap().pop_front().ok_or(())
        }
    }

    #[derive(Default)]
    struct TestStore {
        state: Mutex<Option<AdoptionState>>,
        answer: Mutex<Option<String>>,
        credential: Mutex<Option<Credential>>,
        fail_credential: AtomicBool,
        fail_answer: AtomicBool,
        fail_state: AtomicBool,
        fail_state_call: AtomicUsize,
        state_calls: AtomicUsize,
        writes: Mutex<Vec<&'static str>>,
    }

    impl MigrationStore for TestStore {
        fn load_state(&self) -> Result<Option<AdoptionState>, ()> {
            Ok(self.state.lock().unwrap().clone())
        }

        fn persist_state(&self, state: &AdoptionState) -> Result<(), ()> {
            let call = self.state_calls.fetch_add(1, Ordering::AcqRel) + 1;
            if self.fail_state.load(Ordering::Acquire)
                || self.fail_state_call.load(Ordering::Acquire) == call
            {
                return Err(());
            }
            self.writes.lock().unwrap().push("state");
            *self.state.lock().unwrap() = Some(state.clone());
            Ok(())
        }

        fn read_answer(&self) -> Result<Option<String>, ()> {
            Ok(self.answer.lock().unwrap().clone())
        }

        fn adopt_initial<'a>(
            &'a self,
            marker_digest: &'a str,
            pairing_id: &'a str,
        ) -> StoreFuture<'a> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap().clone().unwrap_or_default();
                if state.adopted_marker_digest.is_some() || state.pending.is_some() {
                    return Err(());
                }
                if self
                    .answer
                    .lock()
                    .unwrap()
                    .as_deref()
                    .is_some_and(|answer| answer != pairing_id)
                {
                    return Err(());
                }
                state.adopted_marker_digest = Some(marker_digest.to_owned());
                self.persist_state(&state)
            })
        }

        fn persist_credential(&self, credential: &Credential) -> Result<(), ()> {
            if self.fail_credential.load(Ordering::Acquire) {
                return Err(());
            }
            self.writes.lock().unwrap().push("credential");
            *self.credential.lock().unwrap() = Some(credential.clone());
            Ok(())
        }

        fn transfer_answer<'a>(
            &'a self,
            old_pairing_id: &'a str,
            new_pairing_id: &'a str,
            previously_confirmed: bool,
        ) -> StoreFuture<'a> {
            Box::pin(async move {
                if self.fail_answer.load(Ordering::Acquire) {
                    return Err(());
                }
                let mut answer = self.answer.lock().unwrap();
                match answer.as_deref() {
                    Some(value) if value == new_pairing_id => Ok(()),
                    Some(value) if value == old_pairing_id && previously_confirmed => {
                        self.writes.lock().unwrap().push("answer");
                        *answer = Some(new_pairing_id.to_owned());
                        Ok(())
                    }
                    None if !previously_confirmed => Ok(()),
                    _ => Err(()),
                }
            })
        }
    }

    struct TestCa {
        cert: Certificate,
        key: KeyPair,
    }

    fn test_ca() -> TestCa {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
        ];
        let cert = params.self_signed(&key).unwrap();
        TestCa { cert, key }
    }

    fn credential(ca: &TestCa) -> Credential {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .extended_key_usages
            .push(ExtendedKeyUsagePurpose::ClientAuth);
        let cert = params.signed_by(&key, &ca.cert, &ca.key).unwrap();
        let ca_der = spl_transport::tls::parse_certs(&ca.cert.pem())
            .unwrap()
            .remove(0);
        let jid = spl_core::relay_window::jid_from_spki(
            &spl_core::ca::extract_spki_der(ca_der.as_ref()).unwrap(),
        )
        .unwrap();
        Credential {
            client_key_pem: key.serialize_pem(),
            client_cert_pem: cert.pem(),
            ca_chain_pem: vec![ca.cert.pem()],
            ca_fp_prefix: spl_core::ca::sha256(ca_der.as_ref())[..16].to_vec(),
            instance_id: jid,
            home_label: "test journal".into(),
            endpoints: vec![EndpointAddr {
                host: "127.0.0.1".into(),
                port: 7657,
            }],
            home_attestation: None,
            local_endpoints: None,
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        }
    }

    #[derive(Clone)]
    struct ServerOperation {
        operation_id: String,
        previous_cid: String,
        response: Vec<u8>,
        candidate_cid: String,
        decision_id: Option<String>,
        decided: bool,
    }

    struct TestTransport {
        ca: TestCa,
        operation: Mutex<Option<ServerOperation>>,
        lose_next_post_response: AtomicBool,
        lose_next_put_response: AtomicBool,
        state_status: AtomicUsize,
        state_body_override: Mutex<Option<Vec<u8>>>,
        rekey_status: AtomicUsize,
        decision_status: AtomicUsize,
        post_bodies: Mutex<Vec<Vec<u8>>>,
        relay_access: Mutex<Option<Value>>,
    }

    impl TestTransport {
        fn new(ca: TestCa) -> Self {
            Self {
                ca,
                operation: Mutex::new(None),
                lose_next_post_response: AtomicBool::new(false),
                lose_next_put_response: AtomicBool::new(false),
                state_status: AtomicUsize::new(0),
                state_body_override: Mutex::new(None),
                rekey_status: AtomicUsize::new(0),
                decision_status: AtomicUsize::new(0),
                post_bodies: Mutex::new(Vec::new()),
                relay_access: Mutex::new(None),
            }
        }

        fn state_body(operation: Option<&ServerOperation>) -> Vec<u8> {
            let body = match operation {
                None => {
                    json!({"protocol_version":1,"rekey_operation_id":null,"previous_cid":null,"state":"none","replaced_cid":null})
                }
                Some(value) => json!({
                    "protocol_version":1,
                    "rekey_operation_id":value.operation_id,
                    "previous_cid":value.previous_cid,
                    "state":if value.decided {"new_device"} else {"pending"},
                    "replaced_cid":null
                }),
            };
            serde_json::to_vec(&body).unwrap()
        }

        fn issue_response(&self, old: &Credential, body: &[u8]) -> Result<(Vec<u8>, String), ()> {
            let request: Value = serde_json::from_slice(body).map_err(|_| ())?;
            let operation_id = request
                .get("operation_id")
                .and_then(Value::as_str)
                .ok_or(())?
                .to_owned();
            let csr = request.get("csr").and_then(Value::as_str).ok_or(())?;
            let csr = CertificateSigningRequestParams::from_pem(csr).map_err(|_| ())?;
            let cert = csr.signed_by(&self.ca.cert, &self.ca.key).map_err(|_| ())?;
            let cid = format!("sha256:{}", spl_core::ca::sha256_hex(cert.der()));
            let old_cid = credential_cid(old).map_err(|_| ())?;
            let mut pairing = json!({
                "client_cert":cert.pem(),
                "ca_chain":[self.ca.cert.pem()],
                "instance_id":old.instance_id,
                "home_label":old.home_label,
                "fingerprint":cid,
                "home_attestation":null,
                "local_endpoints":null
            });
            if let Some(access) = self.relay_access.lock().unwrap().clone() {
                pairing["relay_access"] = access;
            }
            Ok((
                serde_json::to_vec(&json!({
                    "protocol_version":1,"operation_id":operation_id,"state":"pending",
                    "previous_cid":old_cid,"cid":cid,"pairing":pairing
                }))
                .unwrap(),
                cid,
            ))
        }
    }

    impl MigrationTransport for TestTransport {
        fn request<'a>(
            &'a self,
            credential: &'a Credential,
            method: &'a str,
            path: &'a str,
            body: &'a [u8],
        ) -> RequestFuture<'a> {
            Box::pin(async move {
                let old_cid = credential_cid(credential).map_err(|_| ())?;
                let mut operation = self.operation.lock().unwrap();
                if method == "GET" && path == MIGRATION_PATH {
                    let status = self.state_status.load(Ordering::Acquire);
                    if status != 0 {
                        return Ok(MigrationResponse {
                            status: status as u16,
                            body: Vec::new(),
                        });
                    }
                    if let Some(body) = self.state_body_override.lock().unwrap().clone() {
                        return Ok(MigrationResponse { status: 200, body });
                    }
                    // Like the journal, a caller sees the operation that issued
                    // its own certificate, never one it started.
                    let issued = operation
                        .as_ref()
                        .filter(|value| value.candidate_cid == old_cid);
                    return Ok(MigrationResponse {
                        status: 200,
                        body: Self::state_body(issued),
                    });
                }
                if method == "POST" && path == REKEY_PATH {
                    self.post_bodies.lock().unwrap().push(body.to_vec());
                    let status = self.rekey_status.load(Ordering::Acquire);
                    if status != 0 {
                        return Ok(MigrationResponse {
                            status: status as u16,
                            body: Vec::new(),
                        });
                    }
                    let request: Value = serde_json::from_slice(body).map_err(|_| ())?;
                    let operation_id = request
                        .get("operation_id")
                        .and_then(Value::as_str)
                        .ok_or(())?;
                    let response = if let Some(existing) = operation
                        .as_ref()
                        .filter(|value| value.operation_id == operation_id)
                    {
                        (existing.response.clone(), existing.candidate_cid.clone())
                    } else {
                        let result = self.issue_response(credential, body)?;
                        let response_value: Value =
                            serde_json::from_slice(&result.0).map_err(|_| ())?;
                        let previous_cid = response_value
                            .get("previous_cid")
                            .and_then(Value::as_str)
                            .ok_or(())?
                            .to_owned();
                        *operation = Some(ServerOperation {
                            operation_id: operation_id.to_owned(),
                            previous_cid,
                            response: result.0.clone(),
                            candidate_cid: result.1.clone(),
                            decision_id: None,
                            decided: false,
                        });
                        result
                    };
                    if self.lose_next_post_response.swap(false, Ordering::AcqRel) {
                        return Err(());
                    }
                    return Ok(MigrationResponse {
                        status: 201,
                        body: response.0,
                    });
                }
                if method == "PUT" && path == MIGRATION_PATH {
                    let status = self.decision_status.load(Ordering::Acquire);
                    if status != 0 {
                        return Ok(MigrationResponse {
                            status: status as u16,
                            body: Vec::new(),
                        });
                    }
                    let current = operation.as_mut().ok_or(())?;
                    if old_cid != current.candidate_cid {
                        return Err(());
                    }
                    let request: Value = serde_json::from_slice(body).map_err(|_| ())?;
                    let decision_id = request
                        .get("operation_id")
                        .and_then(Value::as_str)
                        .ok_or(())?
                        .to_owned();
                    current.decision_id = Some(decision_id.clone());
                    current.decided = true;
                    let response = serde_json::to_vec(&json!({
                        "protocol_version":1,"operation_id":decision_id,"state":"new_device",
                        "previous_cid":current.previous_cid,"cid":current.candidate_cid,
                        "replaced_cid":null,"display_label":"test journal"
                    }))
                    .unwrap();
                    if self.lose_next_put_response.swap(false, Ordering::AcqRel) {
                        return Err(());
                    }
                    return Ok(MigrationResponse {
                        status: 200,
                        body: response,
                    });
                }
                Err(())
            })
        }
    }

    fn ids(values: &[&str]) -> TestIds {
        TestIds::new(values)
    }

    #[tokio::test]
    async fn first_adoption_persists_only_digest_and_preserves_absent_answer() {
        let ca = test_ca();
        let credential = credential(&ca);
        let store = TestStore::default();
        let marker_text = "test-machine-secret";
        let result = resolve(
            &store,
            &TestMarker(MachineMarker::Present(marker_text.into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            credential.clone(),
        )
        .await
        .unwrap();
        assert_eq!(result, credential);
        assert!(store.answer.lock().unwrap().is_none());
        let state = store.load_state().unwrap().unwrap();
        assert_eq!(
            state.adopted_marker_digest.as_deref(),
            Some(marker_digest(MachineMarker::Present(marker_text.into())).as_str())
        );
        assert!(!serde_json::to_string(&state).unwrap().contains(marker_text));
    }

    #[tokio::test]
    async fn marker_read_failure_fails_closed_without_publishing_state() {
        let ca = test_ca();
        let store = TestStore::default();
        let result = resolve(
            &store,
            &UnavailableMarker,
            &ids(&[]),
            &TestClock,
            &NoRequests,
            credential(&ca),
        )
        .await;
        assert_eq!(result.unwrap_err(), MigrationError::Marker);
        assert!(store.load_state().unwrap().is_none());
        assert!(store.writes.lock().unwrap().is_empty());
    }

    #[test]
    fn missing_and_present_machine_markers_have_distinct_domain_digests() {
        assert_ne!(
            marker_digest(MachineMarker::Missing),
            marker_digest(MachineMarker::Present(String::new()))
        );
    }

    #[tokio::test]
    async fn missing_marker_against_an_adopted_machine_starts_a_move() {
        let ca = test_ca();
        let old = credential(&ca);
        let old_pairing_id = crate::private_link::compute_pairing_id(&old.client_cert_pem);
        let store = TestStore::default();
        *store.answer.lock().unwrap() = Some(old_pairing_id);
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-a".into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();

        let transport = TestTransport::new(ca);
        let moved = resolve(
            &store,
            &TestMarker(MachineMarker::Missing),
            &ids(&[OLD_ID, NEW_DECISION_ID]),
            &TestClock,
            &transport,
            old.clone(),
        )
        .await
        .unwrap();
        assert_ne!(moved.client_key_pem, old.client_key_pem);
        assert_eq!(transport.post_bodies.lock().unwrap().len(), 1);
        let state = store.load_state().unwrap().unwrap();
        assert_eq!(
            state.adopted_marker_digest.as_deref(),
            Some(marker_digest(MachineMarker::Missing).as_str())
        );
        assert_eq!(
            store.answer.lock().unwrap().as_deref(),
            Some(crate::private_link::compute_pairing_id(&moved.client_cert_pem).as_str())
        );
    }

    struct NoRequests;
    impl MigrationTransport for NoRequests {
        fn request<'a>(
            &'a self,
            _: &'a Credential,
            _: &'a str,
            _: &'a str,
            _: &'a [u8],
        ) -> RequestFuture<'a> {
            Box::pin(async { panic!("baseline adoption must not make a network request") })
        }
    }

    #[tokio::test]
    async fn changed_marker_replays_exact_request_after_post_and_put_response_loss() {
        let ca = test_ca();
        let old = credential(&ca);
        let pairing_id = crate::private_link::compute_pairing_id(&old.client_cert_pem);
        let store = TestStore::default();
        *store.answer.lock().unwrap() = Some(pairing_id);
        let marker_a = TestMarker(MachineMarker::Present("machine-a".into()));
        resolve(
            &store,
            &marker_a,
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();

        let transport = TestTransport::new(ca);
        transport
            .lose_next_post_response
            .store(true, Ordering::Release);
        let op_ids = ids(&[OLD_ID, NEW_DECISION_ID]);
        assert!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("machine-b".into())),
                &op_ids,
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .is_err()
        );
        let pending_before = store.load_state().unwrap().unwrap().pending.unwrap();
        let first_request = pending_before.request_bytes.clone();
        let first_candidate_key = pending_before.candidate_key_pem.clone();
        transport
            .lose_next_put_response
            .store(true, Ordering::Release);
        // The first call consumed operation and decision UUIDs even though its POST
        // response was lost. These are supplied from the same deterministic stream.
        let lost_put = resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-b".into())),
            &ids(&[COPIED_DECISION_ID]),
            &TestClock,
            &transport,
            old.clone(),
        )
        .await;
        assert_eq!(lost_put.unwrap_err(), MigrationError::Transport);
        let result = resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-b".into())),
            &ids(&[]),
            &TestClock,
            &transport,
            old,
        )
        .await
        .unwrap();
        let posts = transport.post_bodies.lock().unwrap().clone();
        assert!(posts.len() >= 2);
        assert_eq!(posts[0], first_request);
        assert_eq!(posts[1], first_request);
        let writes = store.writes.lock().unwrap().clone();
        let credential_index = writes
            .iter()
            .position(|item| *item == "credential")
            .unwrap();
        let answer_index = writes.iter().position(|item| *item == "answer").unwrap();
        let adopted_index = writes.iter().rposition(|item| *item == "state").unwrap();
        assert!(credential_index < answer_index && answer_index < adopted_index);
        assert_eq!(first_candidate_key, result.client_key_pem);
    }

    struct SteppedClock(std::sync::atomic::AtomicI64);

    impl MigrationClock for SteppedClock {
        fn unix_seconds(&self) -> i64 {
            self.0.load(Ordering::Acquire)
        }
    }

    fn relay_access(instance_id: &str, iat: i64, exp: i64) -> Value {
        let claims = json!({
            "iss":"https://relay.example.com","sub":format!("instance:{instance_id}"),
            "aud":"spl-relay","scope":"session.dial","ver":2,"instance_id":instance_id,
            "iat":iat,"exp":exp,"jti":"migration-relay-access"
        });
        let token = format!(
            "{}.{}.signature",
            crate::private_link::tests::base64url_no_pad(b"{\"alg\":\"none\"}"),
            crate::private_link::tests::base64url_no_pad(&serde_json::to_vec(&claims).unwrap())
        );
        json!({
            "protocol_version":2,"status":"ready","relay_origin":"https://relay.example.com",
            "instance_id":instance_id,"device_token":token,
            "expires_at":chrono::DateTime::from_timestamp(exp, 0)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
    }

    // The journal replays the relay access it issued with the rekey, and a
    // saved rekey reply is resumed after any outage, so the issued token can
    // expire first. The move still completes; relay renewal refreshes it.
    #[tokio::test]
    async fn rekey_reply_stays_recoverable_after_its_relay_access_expires() {
        const ISSUED_AT: i64 = 1_800_000_000;
        const EXPIRES_AT: i64 = ISSUED_AT + 600;
        for lose_put in [true, false] {
            let ca = test_ca();
            let old = credential(&ca);
            let store = TestStore::default();
            *store.answer.lock().unwrap() = Some(crate::private_link::compute_pairing_id(
                &old.client_cert_pem,
            ));
            let clock = SteppedClock(std::sync::atomic::AtomicI64::new(ISSUED_AT));
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("machine-a".into())),
                &ids(&[]),
                &clock,
                &NoRequests,
                old.clone(),
            )
            .await
            .unwrap();

            let transport = TestTransport::new(ca);
            *transport.relay_access.lock().unwrap() =
                Some(relay_access(&old.instance_id, ISSUED_AT, EXPIRES_AT));
            if lose_put {
                transport
                    .lose_next_put_response
                    .store(true, Ordering::Release);
            } else {
                transport
                    .lose_next_post_response
                    .store(true, Ordering::Release);
            }
            let machine_b = TestMarker(MachineMarker::Present("machine-b".into()));
            let op_ids = ids(&[OLD_ID, NEW_DECISION_ID]);
            assert_eq!(
                resolve(&store, &machine_b, &op_ids, &clock, &transport, old.clone())
                    .await
                    .unwrap_err(),
                MigrationError::Transport
            );
            let saved = store.load_state().unwrap().unwrap().pending.unwrap();
            assert_eq!(saved.rekey_response_bytes.is_some(), lose_put);

            clock.0.store(EXPIRES_AT + 86_400, Ordering::Release);
            let moved = resolve(&store, &machine_b, &op_ids, &clock, &transport, old.clone())
                .await
                .unwrap();
            assert_eq!(moved.client_key_pem, saved.candidate_key_pem);
            assert_eq!(moved.device_token_expires_at, Some(EXPIRES_AT));
            assert_eq!(
                moved.relay_origin.as_deref(),
                Some("https://relay.example.com")
            );
            assert_eq!(store.credential.lock().unwrap().as_ref(), Some(&moved));
            assert!(store.load_state().unwrap().unwrap().pending.is_none());
        }
    }

    #[tokio::test]
    async fn new_device_get_reconciliation_rejects_replaced_cid_then_accepts_null() {
        let ca = test_ca();
        let old = credential(&ca);
        let old_pairing_id = crate::private_link::compute_pairing_id(&old.client_cert_pem);
        let store = TestStore::default();
        *store.answer.lock().unwrap() = Some(old_pairing_id);
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-a".into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();

        let transport = TestTransport::new(ca);
        transport
            .lose_next_put_response
            .store(true, Ordering::Release);
        assert_eq!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("machine-b".into())),
                &ids(&[OLD_ID, NEW_DECISION_ID]),
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Transport
        );
        let operation = transport.operation.lock().unwrap().clone().unwrap();
        let inconsistent_states = [
            json!({
                "protocol_version":1,
                "rekey_operation_id":operation.operation_id,
                "previous_cid":operation.previous_cid,
                "state":"new_device",
                "replaced_cid":format!("sha256:{}", "d".repeat(64))
            }),
            json!({
                "protocol_version":1,
                "rekey_operation_id":NEW_ID,
                "previous_cid":operation.previous_cid,
                "state":"new_device",
                "replaced_cid":null
            }),
            json!({
                "protocol_version":1,
                "rekey_operation_id":operation.operation_id,
                "previous_cid":format!("sha256:{}", "e".repeat(64)),
                "state":"new_device",
                "replaced_cid":null
            }),
        ];
        for state in inconsistent_states {
            *transport.state_body_override.lock().unwrap() =
                Some(serde_json::to_vec(&state).unwrap());
            assert_eq!(
                resolve(
                    &store,
                    &TestMarker(MachineMarker::Present("machine-b".into())),
                    &ids(&[]),
                    &TestClock,
                    &transport,
                    old.clone(),
                )
                .await
                .unwrap_err(),
                MigrationError::Protocol
            );
        }
        assert!(store.credential.lock().unwrap().is_none());

        *transport.state_body_override.lock().unwrap() = None;
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-b".into())),
            &ids(&[]),
            &TestClock,
            &transport,
            old,
        )
        .await
        .unwrap();
        assert!(store.credential.lock().unwrap().is_some());
    }

    #[tokio::test]
    async fn copied_pending_operation_is_dropped_for_a_fresh_candidate_and_operation() {
        let ca = test_ca();
        let old = credential(&ca);
        let original_key = old.client_key_pem.clone();
        let pairing_id = crate::private_link::compute_pairing_id(&old.client_cert_pem);
        let store = TestStore::default();
        *store.answer.lock().unwrap() = Some(pairing_id);
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-a".into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();
        let transport = TestTransport::new(ca);
        transport
            .lose_next_post_response
            .store(true, Ordering::Release);
        let saved_ids = ids(&[OLD_ID, NEW_ID, NEW_DECISION_ID]);
        assert!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("machine-b".into())),
                &saved_ids,
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .is_err()
        );
        let copied = store.load_state().unwrap().unwrap().pending.unwrap();

        let result = resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-copied".into())),
            &saved_ids,
            &TestClock,
            &transport,
            old,
        )
        .await
        .unwrap();
        // The copied operation is neither replayed nor closed with its key.
        let posts = transport.post_bodies.lock().unwrap();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0], copied.request_bytes);
        assert_ne!(posts[1], copied.request_bytes);
        assert_ne!(result.client_key_pem, copied.candidate_key_pem);
        assert_ne!(result.client_key_pem, original_key);
        let copied_request: Value = serde_json::from_slice(&copied.request_bytes).unwrap();
        let fresh_request: Value = serde_json::from_slice(&posts[1]).unwrap();
        assert_ne!(
            fresh_request["operation_id"],
            copied_request["operation_id"]
        );
        assert_ne!(fresh_request["csr"], copied_request["csr"]);
        let state = store.load_state().unwrap().unwrap();
        assert!(state.pending.is_none());
        assert_eq!(
            state.adopted_marker_digest.as_deref(),
            Some(marker_digest(MachineMarker::Present("machine-copied".into())).as_str())
        );
    }

    #[tokio::test]
    async fn explicit_invalid_answer_refuses_before_any_migration_request() {
        let ca = test_ca();
        let old = credential(&ca);
        let store = TestStore::default();
        *store.state.lock().unwrap() = Some(AdoptionState {
            version: 1,
            adopted_marker_digest: Some(marker_digest(MachineMarker::Present("machine-a".into()))),
            pending: None,
            setup_pairing_id: None,
        });
        *store.answer.lock().unwrap() = Some(String::new());
        let transport = TestTransport::new(ca);
        let result = resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-b".into())),
            &ids(&[OLD_ID, COPIED_DECISION_ID]),
            &TestClock,
            &transport,
            old,
        )
        .await;
        assert_eq!(result.unwrap_err(), MigrationError::Answer);
        assert!(transport.post_bodies.lock().unwrap().is_empty());
        assert!(store.load_state().unwrap().unwrap().pending.is_none());
    }

    #[tokio::test]
    async fn legacy_absent_answer_stays_absent_and_same_marker_reinstall_is_paired() {
        let ca = test_ca();
        let old = credential(&ca);
        let store = TestStore::default();
        resolve(
            &store,
            &TestMarker(MachineMarker::Missing),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();
        let same_marker = TestMarker(MachineMarker::Missing);
        assert_eq!(
            resolve(
                &store,
                &same_marker,
                &ids(&[]),
                &TestClock,
                &NoRequests,
                old.clone(),
            )
            .await
            .unwrap(),
            old
        );
        let transport = TestTransport::new(ca);
        let moved = resolve(
            &store,
            &TestMarker(MachineMarker::Present("reinstalled-machine-id".into())),
            &ids(&[OLD_ID, NEW_DECISION_ID]),
            &TestClock,
            &transport,
            old,
        )
        .await
        .unwrap();
        assert!(store.answer.lock().unwrap().is_none());
        let post_count = transport.post_bodies.lock().unwrap().len();
        assert_eq!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("reinstalled-machine-id".into())),
                &ids(&[]),
                &TestClock,
                &transport,
                moved.clone(),
            )
            .await
            .unwrap(),
            moved
        );
        assert_eq!(transport.post_bodies.lock().unwrap().len(), post_count);
    }

    #[tokio::test]
    async fn file_store_move_preserves_absent_legacy_answer_after_setup_gate() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let ca = test_ca();
        let old = credential(&ca);
        let store = FileMigrationStore::new(root);
        assert!(store.load_state().unwrap().is_none());

        resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-a".into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();
        assert!(store.read_answer().unwrap().is_none());

        let transport = TestTransport::new(ca);
        transport
            .lose_next_post_response
            .store(true, Ordering::Release);
        assert_eq!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("machine-b".into())),
                &ids(&[OLD_ID]),
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Transport
        );
        assert!(!should_grandfather_setup_answer_at(root).unwrap());
        assert!(store.read_answer().unwrap().is_none());

        let moved = resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-b".into())),
            &ids(&[NEW_DECISION_ID]),
            &TestClock,
            &transport,
            old.clone(),
        )
        .await
        .unwrap();

        assert_ne!(moved.client_cert_pem, old.client_cert_pem);
        assert_eq!(
            crate::private_link::load_credential(root).unwrap(),
            Some(moved)
        );
        assert!(store.read_answer().unwrap().is_none());
        let state = store.load_state().unwrap().unwrap();
        assert!(state.pending.is_none());
        assert!(state.adopted_marker_digest.is_some());
    }

    #[tokio::test]
    async fn file_store_answer_transfer_requires_old_confirmation_provenance() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let old_pairing_id = "old-pairing";
        let new_pairing_id = "new-pairing";
        let store = FileMigrationStore::new(root);
        store.persist_state(&AdoptionState::default()).unwrap();

        crate::journal_mark::write_pairing_answer(root, old_pairing_id).unwrap();
        assert!(
            store
                .transfer_answer(old_pairing_id, new_pairing_id, false)
                .await
                .is_err()
        );
        assert_eq!(
            store.read_answer().unwrap().as_deref(),
            Some(old_pairing_id)
        );

        store
            .transfer_answer(old_pairing_id, new_pairing_id, true)
            .await
            .unwrap();
        assert_eq!(
            store.read_answer().unwrap().as_deref(),
            Some(new_pairing_id)
        );

        crate::journal_mark::write_pairing_answer(root, "").unwrap();
        assert!(
            store
                .transfer_answer(old_pairing_id, new_pairing_id, true)
                .await
                .is_err()
        );
        assert_eq!(store.read_answer().unwrap().as_deref(), Some(""));

        crate::journal_mark::write_pairing_answer(root, "unrelated-pairing").unwrap();
        assert!(
            store
                .transfer_answer(old_pairing_id, new_pairing_id, true)
                .await
                .is_err()
        );
        assert_eq!(
            store.read_answer().unwrap().as_deref(),
            Some("unrelated-pairing")
        );

        std::fs::write(
            root.join(crate::journal_mark::PAIRING_ANSWER_FILENAME),
            b"not-json",
        )
        .unwrap();
        assert!(
            store
                .transfer_answer(old_pairing_id, new_pairing_id, true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_answer_publication_keeps_old_confirmed_provenance_for_recovery() {
        let ca = test_ca();
        let old = credential(&ca);
        let pairing_id = crate::private_link::compute_pairing_id(&old.client_cert_pem);
        let store = TestStore::default();
        *store.answer.lock().unwrap() = Some(pairing_id.clone());
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("old-marker".into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();
        let transport = TestTransport::new(ca);
        let new_marker = TestMarker(MachineMarker::Present("new-marker".into()));
        store.fail_credential.store(true, Ordering::Release);
        assert_eq!(
            resolve(
                &store,
                &new_marker,
                &ids(&[OLD_ID, NEW_DECISION_ID]),
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Persistence
        );
        assert!(store.credential.lock().unwrap().is_none());
        assert_eq!(
            store.answer.lock().unwrap().as_deref(),
            Some(pairing_id.as_str())
        );

        store.fail_credential.store(false, Ordering::Release);
        store.fail_answer.store(true, Ordering::Release);
        assert_eq!(
            resolve(
                &store,
                &new_marker,
                &ids(&[]),
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Answer
        );
        let pending = store.load_state().unwrap().unwrap().pending.unwrap();
        assert!(pending.transfer_confirmed);
        assert_eq!(pending.old_pairing_id, pairing_id);
        assert_eq!(
            store.answer.lock().unwrap().as_deref(),
            Some(pairing_id.as_str())
        );
        let new_credential = store.credential.lock().unwrap().clone().unwrap();

        store.fail_answer.store(false, Ordering::Release);
        let resolved = resolve(
            &store,
            &new_marker,
            &ids(&[]),
            &TestClock,
            &transport,
            new_credential.clone(),
        )
        .await
        .unwrap();
        assert_eq!(resolved, new_credential);
        assert_eq!(
            store.answer.lock().unwrap().as_deref(),
            Some(crate::private_link::compute_pairing_id(&resolved.client_cert_pem).as_str())
        );
        assert!(store.load_state().unwrap().unwrap().pending.is_none());
    }

    #[tokio::test]
    async fn pending_request_must_be_durable_before_first_network_request() {
        let ca = test_ca();
        let old = credential(&ca);
        let pairing_id = crate::private_link::compute_pairing_id(&old.client_cert_pem);
        let store = TestStore::default();
        *store.answer.lock().unwrap() = Some(pairing_id);
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("old-marker".into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();
        let transport = TestTransport::new(ca);
        store.fail_state.store(true, Ordering::Release);
        assert_eq!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("new-marker".into())),
                &ids(&[OLD_ID, NEW_DECISION_ID]),
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Persistence
        );
        assert!(transport.post_bodies.lock().unwrap().is_empty());
        assert!(store.load_state().unwrap().unwrap().pending.is_none());

        store.fail_state.store(false, Ordering::Release);
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("new-marker".into())),
            &ids(&[OLD_ID, NEW_DECISION_ID]),
            &TestClock,
            &transport,
            old,
        )
        .await
        .unwrap();
        assert_eq!(transport.post_bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn pending_migration_cannot_restore_identity_after_new_journal_pairing() {
        let ca = test_ca();
        let old = credential(&ca);
        let pairing_id = crate::private_link::compute_pairing_id(&old.client_cert_pem);
        let store = TestStore::default();
        *store.answer.lock().unwrap() = Some(pairing_id);
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("old-marker".into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();

        let transport = TestTransport::new(ca);
        transport
            .lose_next_post_response
            .store(true, Ordering::Release);
        assert_eq!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("new-marker".into())),
                &ids(&[OLD_ID]),
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Transport
        );

        let newly_paired = credential(&transport.ca);
        *store.credential.lock().unwrap() = Some(newly_paired.clone());
        let state_before = store.load_state().unwrap().unwrap();
        let requests_before = transport.post_bodies.lock().unwrap().len();
        assert_eq!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("new-marker".into())),
                &ids(&[]),
                &TestClock,
                &transport,
                newly_paired.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Identity
        );
        assert_eq!(transport.post_bodies.lock().unwrap().len(), requests_before);
        assert_eq!(*store.credential.lock().unwrap(), Some(newly_paired));
        assert_eq!(store.load_state().unwrap().unwrap(), state_before);
    }

    #[tokio::test]
    async fn setup_finalization_recovers_after_credential_and_answer_are_durable() {
        let ca = test_ca();
        let old = credential(&ca);
        let old_pairing_id = crate::private_link::compute_pairing_id(&old.client_cert_pem);
        let previous_marker = MachineMarker::Present("machine-a".into());
        let current_marker = MachineMarker::Present("machine-b".into());
        let pending = create_pending(
            marker_digest(current_marker.clone()),
            Some(marker_digest(previous_marker.clone())),
            old,
            format!("sha256:{}", "a".repeat(64)),
            old_pairing_id,
            true,
            &ids(&[OLD_ID]),
        )
        .unwrap();
        let store = TestStore::default();
        *store.state.lock().unwrap() = Some(AdoptionState {
            version: 1,
            adopted_marker_digest: Some(marker_digest(previous_marker)),
            pending: Some(pending),
            setup_pairing_id: None,
        });

        let new_credential = credential(&ca);
        let new_pairing_id =
            crate::private_link::compute_pairing_id(&new_credential.client_cert_pem);
        prepare_setup_pairing(&store, &new_pairing_id).unwrap();
        store.persist_credential(&new_credential).unwrap();
        *store.answer.lock().unwrap() = Some(new_pairing_id.clone());
        store.fail_state.store(true, Ordering::Release);
        assert!(
            finish_setup_pairing(&store, &TestMarker(current_marker.clone()), &new_pairing_id)
                .is_err()
        );

        let interrupted = store.load_state().unwrap().unwrap();
        assert!(interrupted.pending.is_some());
        assert_eq!(
            interrupted.setup_pairing_id.as_deref(),
            Some(new_pairing_id.as_str())
        );
        store.fail_state.store(false, Ordering::Release);
        resolve(
            &store,
            &TestMarker(current_marker.clone()),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            new_credential.clone(),
        )
        .await
        .unwrap();
        let recovered = store.load_state().unwrap().unwrap();
        assert!(recovered.pending.is_none());
        assert!(recovered.setup_pairing_id.is_none());
        assert_eq!(
            recovered.adopted_marker_digest.as_deref(),
            Some(marker_digest(current_marker).as_str())
        );
        assert_eq!(*store.credential.lock().unwrap(), Some(new_credential));
    }

    #[tokio::test]
    async fn repeated_moves_bind_each_new_operation_to_the_current_certificate() {
        let ca = test_ca();
        let original = credential(&ca);
        let pairing_id = crate::private_link::compute_pairing_id(&original.client_cert_pem);
        let store = TestStore::default();
        *store.answer.lock().unwrap() = Some(pairing_id);
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-a".into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            original.clone(),
        )
        .await
        .unwrap();

        let transport = TestTransport::new(ca);
        let first = resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-b".into())),
            &ids(&[OLD_ID, NEW_DECISION_ID]),
            &TestClock,
            &transport,
            original,
        )
        .await
        .unwrap();
        let first_cid = credential_cid(&first).unwrap();
        let second = resolve(
            &store,
            &TestMarker(MachineMarker::Present("machine-c".into())),
            &ids(&[COPIED_DECISION_ID, "123e4567-e89b-42d3-a456-426614174004"]),
            &TestClock,
            &transport,
            first,
        )
        .await
        .unwrap();

        let operation = transport.operation.lock().unwrap().clone().unwrap();
        assert_eq!(operation.previous_cid, first_cid);
        assert_ne!(operation.candidate_cid, first_cid);
        assert_eq!(transport.post_bodies.lock().unwrap().len(), 2);
        assert!(store.load_state().unwrap().unwrap().pending.is_none());
        assert_eq!(
            store
                .load_state()
                .unwrap()
                .unwrap()
                .adopted_marker_digest
                .as_deref(),
            Some(marker_digest(MachineMarker::Present("machine-c".into())).as_str())
        );
        assert_eq!(
            store.answer.lock().unwrap().as_deref(),
            Some(crate::private_link::compute_pairing_id(&second.client_cert_pem).as_str())
        );
    }

    #[tokio::test]
    async fn protocol_refusals_and_phase_checkpoint_failures_remain_recoverable() {
        for failed_state_write in [3, 4, 5, 6] {
            let ca = test_ca();
            let old = credential(&ca);
            let pairing_id = crate::private_link::compute_pairing_id(&old.client_cert_pem);
            let store = TestStore::default();
            *store.answer.lock().unwrap() = Some(pairing_id);
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("old-marker".into())),
                &ids(&[]),
                &TestClock,
                &NoRequests,
                old.clone(),
            )
            .await
            .unwrap();
            store
                .fail_state_call
                .store(failed_state_write, Ordering::Release);
            let transport = TestTransport::new(ca);
            assert_eq!(
                resolve(
                    &store,
                    &TestMarker(MachineMarker::Present("new-marker".into())),
                    &ids(&[OLD_ID, NEW_DECISION_ID]),
                    &TestClock,
                    &transport,
                    old.clone(),
                )
                .await
                .unwrap_err(),
                MigrationError::Persistence
            );
            assert_eq!(transport.post_bodies.lock().unwrap().len(), 1);
            let pending = store.load_state().unwrap().unwrap().pending.unwrap();
            assert_eq!(
                store
                    .load_state()
                    .unwrap()
                    .unwrap()
                    .adopted_marker_digest
                    .as_deref(),
                Some(marker_digest(MachineMarker::Present("old-marker".into())).as_str())
            );
            if failed_state_write == 3 {
                assert!(pending.rekey_response_bytes.is_none());
            } else {
                assert!(pending.rekey_response_bytes.is_some());
            }
            if failed_state_write >= 5 {
                assert!(pending.decision_id.is_some());
                assert!(pending.decision_bytes.is_some());
            }
            if failed_state_write == 6 {
                assert!(pending.decision_response_bytes.is_some());
                assert!(store.credential.lock().unwrap().is_some());
            } else {
                assert!(store.credential.lock().unwrap().is_none());
            }

            store.fail_state_call.store(0, Ordering::Release);
            let current = store.credential.lock().unwrap().clone().unwrap_or(old);
            let uuid_values = if failed_state_write <= 4 {
                &["123e4567-e89b-42d3-a456-426614174005"][..]
            } else {
                &[][..]
            };
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("new-marker".into())),
                &ids(uuid_values),
                &TestClock,
                &transport,
                current,
            )
            .await
            .unwrap();
            assert!(store.load_state().unwrap().unwrap().pending.is_none());
            assert_eq!(
                store
                    .load_state()
                    .unwrap()
                    .unwrap()
                    .adopted_marker_digest
                    .as_deref(),
                Some(marker_digest(MachineMarker::Present("new-marker".into())).as_str())
            );
        }

        let ca = test_ca();
        let old = credential(&ca);
        let store = TestStore::default();
        *store.answer.lock().unwrap() = Some(crate::private_link::compute_pairing_id(
            &old.client_cert_pem,
        ));
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("old-marker".into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();
        let transport = TestTransport::new(ca);
        transport.state_status.store(404, Ordering::Release);
        assert_eq!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("new-marker".into())),
                &ids(&[OLD_ID, NEW_DECISION_ID]),
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Transport
        );
        assert!(transport.post_bodies.lock().unwrap().is_empty());
        transport.state_status.store(0, Ordering::Release);
        transport.rekey_status.store(400, Ordering::Release);
        assert_eq!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("new-marker".into())),
                &ids(&[]),
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Protocol
        );
        let saved = store.load_state().unwrap().unwrap().pending.unwrap();
        transport.rekey_status.store(404, Ordering::Release);
        assert_eq!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("new-marker".into())),
                &ids(&[]),
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Protocol
        );
        transport.rekey_status.store(0, Ordering::Release);
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("new-marker".into())),
            &ids(&[NEW_DECISION_ID]),
            &TestClock,
            &transport,
            old,
        )
        .await
        .unwrap();
        let posts = transport.post_bodies.lock().unwrap().clone();
        assert_eq!(posts.len(), 3);
        assert_eq!(posts[1], saved.request_bytes);
        assert_eq!(posts[2], saved.request_bytes);

        let ca = test_ca();
        let old = credential(&ca);
        let pairing_id = crate::private_link::compute_pairing_id(&old.client_cert_pem);
        let store = TestStore::default();
        *store.answer.lock().unwrap() = Some(pairing_id);
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("old-marker".into())),
            &ids(&[]),
            &TestClock,
            &NoRequests,
            old.clone(),
        )
        .await
        .unwrap();
        let transport = TestTransport::new(ca);
        transport.decision_status.store(404, Ordering::Release);
        assert_eq!(
            resolve(
                &store,
                &TestMarker(MachineMarker::Present("new-marker".into())),
                &ids(&[OLD_ID, NEW_DECISION_ID]),
                &TestClock,
                &transport,
                old.clone(),
            )
            .await
            .unwrap_err(),
            MigrationError::Protocol
        );
        let pending = store.load_state().unwrap().unwrap().pending.unwrap();
        assert!(pending.decision_id.is_some());
        assert!(pending.decision_bytes.is_some());
        assert!(pending.decision_response_bytes.is_none());
        assert!(
            !transport
                .operation
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .decided
        );
        transport.decision_status.store(0, Ordering::Release);
        resolve(
            &store,
            &TestMarker(MachineMarker::Present("new-marker".into())),
            &ids(&[]),
            &TestClock,
            &transport,
            old,
        )
        .await
        .unwrap();
        assert!(store.load_state().unwrap().unwrap().pending.is_none());
    }

    // The pinned contract vectors run through the typed parsers: each valid
    // vector is accepted and a closed-shape or binding mutation is refused.
    #[test]
    fn pinned_contract_vectors_pass_typed_parsers_and_mutations_fail() {
        let document: Value = serde_json::from_str(include_str!(
            "../../../contracts/device-migration/v1.vectors.json"
        ))
        .unwrap();
        let vectors = &document["vectors"];
        let bytes = |value: &Value| serde_json::to_vec(value).unwrap();
        let mutated = |value: &Value, pointer: &str, replacement: Option<Value>| {
            let mut value = value.clone();
            let (parent, field) = pointer.rsplit_once('/').unwrap();
            let object = value.pointer_mut(parent).unwrap().as_object_mut().unwrap();
            match replacement {
                Some(replacement) => object.insert(field.to_owned(), replacement),
                None => object.remove(field),
            };
            bytes(&value)
        };

        let request = &vectors["rekey_request"];
        assert!(parse_closed::<RekeyRequest>(&bytes(request)).is_ok());
        for (pointer, replacement) in [
            ("/unexpected", Some(json!(true))),
            ("/csr", None),
            ("/platform", Some(json!("plan9"))),
        ] {
            assert!(
                parse_closed::<RekeyRequest>(&mutated(request, pointer, replacement)).is_err(),
                "{pointer}"
            );
        }

        let ca = test_ca();
        let operation_id = request["operation_id"].as_str().unwrap();
        let mut pending = create_pending(
            "marker".into(),
            None,
            credential(&ca),
            String::new(),
            String::new(),
            true,
            &ids(&[operation_id]),
        )
        .unwrap();
        for name in [
            "rekey_created_201",
            "rekey_replay_200",
            "rekey_without_network_metadata",
        ] {
            let body = &vectors[name]["body"];
            pending.old_cid = body["previous_cid"].as_str().unwrap().to_owned();
            assert!(parse_rekey(&bytes(body), &pending).is_ok(), "{name}");
            assert!(
                parse_rekey(
                    &mutated(body, "/pairing/future", Some(json!(true))),
                    &pending
                )
                .is_ok(),
                "{name} pairing stays additive"
            );
            for (pointer, replacement) in [
                ("/unexpected", Some(json!(true))),
                ("/state", Some(json!("new_device"))),
                ("/operation_id", Some(json!(NEW_ID))),
                (
                    "/previous_cid",
                    Some(json!(format!("sha256:{}", "c".repeat(64)))),
                ),
                (
                    "/pairing/fingerprint",
                    Some(json!(format!("sha256:{}", "c".repeat(64)))),
                ),
                ("/pairing/client_cert", None),
            ] {
                assert!(
                    parse_rekey(&mutated(body, pointer, replacement), &pending).is_err(),
                    "{name} {pointer}"
                );
            }
        }

        for state in vectors["migration_states"].as_array().unwrap() {
            assert!(parse_migration_state(&bytes(state)).is_ok(), "{state}");
            for (pointer, replacement) in [
                ("/unexpected", Some(json!(true))),
                ("/replaced_cid", None),
                ("/protocol_version", Some(json!(2))),
            ] {
                assert!(
                    parse_migration_state(&mutated(state, pointer, replacement)).is_err(),
                    "{state} {pointer}"
                );
            }
        }

        let decision = &vectors["decision_response"];
        assert!(parse_closed::<DecisionResponse>(&bytes(decision)).is_ok());
        for (pointer, replacement) in [
            ("/unexpected", Some(json!(true))),
            ("/display_label", None),
            ("/state", Some(json!("moved"))),
        ] {
            assert!(
                parse_closed::<DecisionResponse>(&mutated(decision, pointer, replacement)).is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn protocol_messages_validate_operation_cid_and_key_binding() {
        let ca = test_ca();
        let old = credential(&ca);
        let pending = create_pending(
            marker_digest(MachineMarker::Present("new-marker".into())),
            Some(marker_digest(MachineMarker::Present("old-marker".into()))),
            old.clone(),
            credential_cid(&old).unwrap(),
            crate::private_link::compute_pairing_id(&old.client_cert_pem),
            true,
            &ids(&[OLD_ID]),
        )
        .unwrap();
        let transport = TestTransport::new(ca);
        let (valid, _) = transport
            .issue_response(&old, &pending.request_bytes)
            .unwrap();
        assert!(parse_rekey(&valid, &pending).is_ok());

        let mutations: [fn(&mut Value); 5] = [
            |body: &mut Value| body["operation_id"] = json!(NEW_ID),
            |body: &mut Value| body["previous_cid"] = json!("sha256:bad"),
            |body: &mut Value| body["cid"] = json!("sha256:bad"),
            |body: &mut Value| body["pairing"]["fingerprint"] = json!("sha256:bad"),
            |body: &mut Value| body["future_outer"] = json!(true),
        ];
        for mutate in mutations {
            let mut body: Value = serde_json::from_slice(&valid).unwrap();
            mutate(&mut body);
            assert!(parse_rekey(&serde_json::to_vec(&body).unwrap(), &pending).is_err());
        }

        let mut future_pairing_field: Value = serde_json::from_slice(&valid).unwrap();
        future_pairing_field["pairing"]["future_pairing_fact"] = json!("preserved by schema");
        assert!(
            parse_rekey(
                &serde_json::to_vec(&future_pairing_field).unwrap(),
                &pending
            )
            .is_ok()
        );

        let response = parse_rekey(&valid, &pending).unwrap();
        let unrelated_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        assert_eq!(
            credential_from_pairing(
                &old,
                &unrelated_key.serialize_pem(),
                response,
                TestClock.unix_seconds(),
            )
            .unwrap_err(),
            MigrationError::Identity
        );

        let unrelated_ca = test_ca();
        let mut untrusted: Value = serde_json::from_slice(&valid).unwrap();
        untrusted["pairing"]["ca_chain"] = json!([unrelated_ca.cert.pem()]);
        let response = parse_rekey(&serde_json::to_vec(&untrusted).unwrap(), &pending).unwrap();
        assert_eq!(
            credential_from_pairing(
                &old,
                &pending.candidate_key_pem,
                response,
                TestClock.unix_seconds(),
            )
            .unwrap_err(),
            MigrationError::Identity
        );
    }

    #[test]
    fn migration_responses_require_nullable_keys_and_state_consistency() {
        let none_state = json!({
            "protocol_version": 1,
            "rekey_operation_id": null,
            "previous_cid": null,
            "state": "none",
            "replaced_cid": null
        });
        assert!(parse_migration_state(&serde_json::to_vec(&none_state).unwrap()).is_ok());
        for missing in ["rekey_operation_id", "previous_cid", "replaced_cid"] {
            let mut response = none_state.clone();
            response.as_object_mut().unwrap().remove(missing);
            assert!(parse_migration_state(&serde_json::to_vec(&response).unwrap()).is_err());
        }

        let cid = format!("sha256:{}", "a".repeat(64));
        let valid_new_device = json!({
            "protocol_version": 1,
            "rekey_operation_id": OLD_ID,
            "previous_cid": cid,
            "state": "new_device",
            "replaced_cid": null
        });
        assert!(parse_migration_state(&serde_json::to_vec(&valid_new_device).unwrap()).is_ok());
        let mut contradictory = valid_new_device.clone();
        contradictory["replaced_cid"] = json!(format!("sha256:{}", "b".repeat(64)));
        assert!(parse_migration_state(&serde_json::to_vec(&contradictory).unwrap()).is_err());

        let same_device = json!({
            "protocol_version": 1,
            "rekey_operation_id": OLD_ID,
            "previous_cid": &cid,
            "state": "same_device",
            "replaced_cid": &cid
        });
        assert!(parse_migration_state(&serde_json::to_vec(&same_device).unwrap()).is_ok());
        let mut inconsistent_same = same_device.clone();
        inconsistent_same["replaced_cid"] = json!(format!("sha256:{}", "b".repeat(64)));
        assert!(parse_migration_state(&serde_json::to_vec(&inconsistent_same).unwrap()).is_err());

        let replaced_device = json!({
            "protocol_version": 1,
            "rekey_operation_id": null,
            "previous_cid": null,
            "state": "replaced_device",
            "replaced_cid": format!("sha256:{}", "b".repeat(64))
        });
        assert!(parse_migration_state(&serde_json::to_vec(&replaced_device).unwrap()).is_ok());

        let ca = test_ca();
        let old = credential(&ca);
        let mut pending = create_pending(
            marker_digest(MachineMarker::Present("new-marker".into())),
            Some(marker_digest(MachineMarker::Present("old-marker".into()))),
            old.clone(),
            credential_cid(&old).unwrap(),
            crate::private_link::compute_pairing_id(&old.client_cert_pem),
            true,
            &ids(&[OLD_ID]),
        )
        .unwrap();
        pending.candidate_cid = Some(format!("sha256:{}", "c".repeat(64)));
        pending.decision_id = Some(NEW_ID.to_owned());
        let decision = json!({
            "protocol_version": 1,
            "operation_id": NEW_ID,
            "state": "new_device",
            "previous_cid": pending.old_cid,
            "cid": pending.candidate_cid,
            "replaced_cid": null,
            "display_label": "Fixture device"
        });
        assert!(validate_decision(&serde_json::to_vec(&decision).unwrap(), &pending).is_ok());
        for missing in ["previous_cid", "replaced_cid"] {
            let mut response = decision.clone();
            response.as_object_mut().unwrap().remove(missing);
            assert!(validate_decision(&serde_json::to_vec(&response).unwrap(), &pending).is_err());
        }
    }
}
