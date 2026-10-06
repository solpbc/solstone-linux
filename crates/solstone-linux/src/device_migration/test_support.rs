// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! In-memory machine and startup seams for tests outside this module.

use super::*;

/// The one machine every routine test runs on, so setup and startup agree.
pub(crate) struct FixedMarker(pub(crate) &'static str);

impl MarkerProvider for FixedMarker {
    fn read_marker(&self) -> Result<MachineMarker, ()> {
        Ok(MachineMarker::Present(self.0.to_owned()))
    }
}

pub(crate) static TEST_MACHINE: FixedMarker = FixedMarker("private-link-setup-test");

/// The production startup resolver on [`TEST_MACHINE`], with no ids and no
/// network: routine startup adopts or confirms but never migrates.
pub(crate) struct TestMachineResolver;

pub(crate) static TEST_RESOLVER: TestMachineResolver = TestMachineResolver;

impl StartupResolver for TestMachineResolver {
    fn resolve<'a>(&'a self, root: &'a Path, credential: Credential) -> StartupResolveFuture<'a> {
        Box::pin(async move {
            resolve(
                &FileMigrationStore::new(root),
                &TEST_MACHINE,
                &NoIds,
                &FixedClock,
                &NoNetwork,
                credential,
            )
            .await
            .map_err(|error| error.to_string())
        })
    }
}

struct FixedClock;

impl MigrationClock for FixedClock {
    fn unix_seconds(&self) -> i64 {
        1_800_000_000
    }
}

struct NoIds;

impl OperationIdProvider for NoIds {
    fn next_uuid(&self) -> Result<String, ()> {
        Err(())
    }
}

struct NoNetwork;

impl MigrationTransport for NoNetwork {
    fn request<'a>(
        &'a self,
        _: &'a Credential,
        _: &'a str,
        _: &'a str,
        _: &'a [u8],
    ) -> RequestFuture<'a> {
        Box::pin(async { Err(()) })
    }
}

pub(crate) fn seed_pending_for_setup_test(
    root: &Path,
    credential: Credential,
    previously_confirmed: bool,
) -> Result<(), ()> {
    struct SetupTestId;
    impl OperationIdProvider for SetupTestId {
        fn next_uuid(&self) -> Result<String, ()> {
            Ok("123e4567-e89b-42d3-a456-426614174099".to_owned())
        }
    }

    let old_pairing_id = crate::private_link::compute_pairing_id(&credential.client_cert_pem);
    let old_cid = format!(
        "sha256:{}",
        spl_core::ca::sha256_hex(credential.client_cert_pem.as_bytes())
    );
    let previous_marker_digest = marker_digest(MachineMarker::Present("setup-old".to_owned()));
    let pending = create_pending(
        marker_digest(MachineMarker::Present("setup-new".to_owned())),
        Some(previous_marker_digest.clone()),
        credential,
        old_cid,
        old_pairing_id,
        previously_confirmed,
        &SetupTestId,
    )
    .map_err(|_| ())?;
    FileMigrationStore::new(root).persist_state(&AdoptionState {
        version: 1,
        adopted_marker_digest: Some(previous_marker_digest),
        pending: Some(pending),
        setup_pairing_id: None,
    })
}

pub(crate) async fn resolve_setup_pairing_for_test(
    root: &Path,
    credential: Credential,
    marker: MachineMarker,
) -> Result<(), ()> {
    struct TestMarker(MachineMarker);
    impl MarkerProvider for TestMarker {
        fn read_marker(&self) -> Result<MachineMarker, ()> {
            Ok(self.0.clone())
        }
    }

    resolve(
        &FileMigrationStore::new(root),
        &TestMarker(marker),
        &NoIds,
        &FixedClock,
        &NoNetwork,
        credential,
    )
    .await
    .map(|_| ())
    .map_err(|_| ())
}
