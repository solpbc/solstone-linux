// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::fd::AsFd,
    path::Path,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use spl_transport::credential::Credential;

use crate::{
    private_file::{
        DurableWriteFault, NoWriteFault, PrivateFileError, atomic_write_bytes_with_fault,
        ensure_private_directory, open_regular_readonly,
    },
    private_link::{
        PrivateIoOperation, PrivateStateError, PrivateTargetKind, SYSTEM_STATUS_TIMEOUT,
        compute_pairing_id, format_spoken_mark, load_credential, read_private_file,
    },
};

pub(crate) const PAIRING_ANSWER_FILENAME: &str = "pairing-answer.json";
pub(crate) const ANSWER_LOCK_FILENAME: &str = ".solstone-linux.pairing-answer.lock";
pub(crate) const ANSWER_LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const JOURNAL_MARK_RECHECK_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) const HELD_BOTH_SENTENCES: &str = "waiting for you to confirm your journal's mark. nothing waiting goes into your journal until you do.";
#[allow(dead_code)]
pub(crate) const HELD_FIRST_SENTENCE: &str = "waiting for you to confirm your journal's mark";
pub(crate) const RUN_LINE: &str = "when you're ready, run: solstone-linux confirm";
pub(crate) const SUCCESS_LINE: &str = "the solstone app can now connect to your journal.";
pub(crate) const NOT_PAIRED: &str = "not paired.";
pub(crate) const MISMATCH_BODY: &str = "you said this mark doesn't match the one your journal shows, so this computer isn't paired, and nothing it has kept went to that journal through this link. you may have pasted the wrong link, or something isn't right. get a fresh pair link from your journal and run setup again, or email support@solstone.app and we'll help.";
pub(crate) const CANCEL_LINE: &str = "pairing cancelled. nothing this computer has kept went to that journal through this link. get a fresh pair link from your journal and run setup again when you're ready.";
pub(crate) const CONFIRM_DONE: &str = "your journal's mark is already confirmed. nothing to do.";
pub(crate) const CONFIRM_UNPAIRED: &str = "not paired. to pair, run: solstone-linux setup";
pub(crate) const COULDNT_VERIFY: &str = "couldn't verify.";
pub(crate) const SETUP_NO_TERMINAL: &str = "setup can't ask you about your journal's mark here. run it in a terminal, or give the mark's two words with --mark \"word word\".";
pub(crate) const CONFIRM_NO_TERMINAL: &str = "confirm can't ask you about your journal's mark here. run it in a terminal, or give the mark's two words with --mark \"word word\".";
pub(crate) const MARK_USAGE: &str =
    "give the two words of your journal's mark, like --mark \"bramble quokka\".";
pub(crate) const MARK_MISMATCH_LINE: &str = "the mark words you gave don't match the journal's mark, so this computer isn't paired, and nothing it has kept went to that journal through this link. check the words, get a fresh pair link from your journal and run setup again.";
pub(crate) const SETUP_UNVERIFIABLE_LINE: &str = "this computer couldn't work out the journal's mark, so the words you gave can't be matched. this computer isn't paired, and nothing it has kept went to that journal through this link. get a fresh pair link from your journal and run setup in a terminal to decide for yourself.";
pub(crate) const CONFIRM_UNVERIFIABLE_LINE: &str = "this computer couldn't work out the journal's mark, so the words you gave can't be matched. waiting for you to confirm your journal's mark. nothing waiting goes into your journal until you do. run solstone-linux confirm in a terminal to decide for yourself.";
pub(crate) const MARK_HELP: &str = "the two words of the mark your journal's network app shows. needed when there's no terminal to ask you on.";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairingAnswer {
    pub(crate) confirmed: String,
}

pub(crate) struct AnswerLock {
    _file: File,
}

impl AnswerLock {
    pub(crate) async fn acquire(config_root: &Path) -> Result<Self, PrivateStateError> {
        ensure_private_directory(config_root).map_err(|error| {
            map_private_file(
                error,
                PrivateTargetKind::ConfigDirectory,
                PrivateIoOperation::EnsureDirectory,
            )
        })?;
        let canonical_root =
            fs::canonicalize(config_root).map_err(|source| PrivateStateError::Io {
                operation: PrivateIoOperation::Canonicalize,
                source,
            })?;
        let root_descriptor = rustix::fs::openat(
            rustix::fs::CWD,
            &canonical_root,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        )
        .map_err(|source| PrivateStateError::Io {
            operation: PrivateIoOperation::Open,
            source: source.into(),
        })?;
        let descriptor = rustix::fs::openat(
            &root_descriptor,
            ANSWER_LOCK_FILENAME,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CREATE,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map_err(|source| PrivateStateError::Io {
            operation: PrivateIoOperation::Open,
            source: source.into(),
        })?;
        let file = File::from(descriptor);
        if !file
            .metadata()
            .map_err(|source| PrivateStateError::Io {
                operation: PrivateIoOperation::Inspect,
                source,
            })?
            .is_file()
        {
            return Err(PrivateStateError::InvalidTarget {
                kind: PrivateTargetKind::Lock,
            });
        }
        rustix::fs::fchmod(&file, rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR).map_err(
            |source| PrivateStateError::Io {
                operation: PrivateIoOperation::Chmod,
                source: source.into(),
            },
        )?;
        verify_answer_lock(&file)?;

        let deadline = tokio::time::Instant::now() + ANSWER_LOCK_ACQUIRE_TIMEOUT;
        loop {
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(Self { _file: file }),
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(PrivateStateError::Io {
                            operation: PrivateIoOperation::Lock,
                            source: io::Error::new(io::ErrorKind::TimedOut, "answer lock timeout"),
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(source) => {
                    return Err(PrivateStateError::Io {
                        operation: PrivateIoOperation::Lock,
                        source: source.into(),
                    });
                }
            }
        }
    }
}

impl Drop for AnswerLock {
    fn drop(&mut self) {
        if let Err(error) = rustix::fs::flock(&self._file, rustix::fs::FlockOperation::Unlock) {
            tracing::error!(%error, "Failed to release answer lock");
        }
    }
}

fn verify_answer_lock(file: &File) -> Result<(), PrivateStateError> {
    let stat = rustix::fs::fstat(file).map_err(|source| PrivateStateError::Io {
        operation: PrivateIoOperation::Inspect,
        source: source.into(),
    })?;
    let expected_mode = rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
        || rustix::fs::Mode::from_raw_mode(stat.st_mode) != expected_mode
        || stat.st_uid != rustix::process::geteuid().as_raw()
    {
        return Err(PrivateStateError::InvalidTarget {
            kind: PrivateTargetKind::Lock,
        });
    }
    Ok(())
}

fn map_private_file(
    error: PrivateFileError,
    kind: PrivateTargetKind,
    fallback_operation: PrivateIoOperation,
) -> PrivateStateError {
    match error {
        PrivateFileError::InvalidTarget(_) => PrivateStateError::InvalidTarget { kind },
        PrivateFileError::Io {
            operation, kind, ..
        } => PrivateStateError::Io {
            operation: match operation {
                "open" => PrivateIoOperation::Open,
                "read" => PrivateIoOperation::Read,
                "chmod" => PrivateIoOperation::Chmod,
                "inspect" => PrivateIoOperation::Inspect,
                "create" => PrivateIoOperation::Persist,
                "write" => PrivateIoOperation::Persist,
                "fsync" => PrivateIoOperation::Persist,
                "rename" => PrivateIoOperation::Persist,
                _ => fallback_operation,
            },
            source: io::Error::from(kind),
        },
    }
}

pub(crate) fn read_pairing_answer(
    config_root: &Path,
) -> Result<Option<PairingAnswer>, PrivateStateError> {
    let path = config_root.join(PAIRING_ANSWER_FILENAME);
    let bytes = match read_private_file(&path, PrivateTargetKind::Credential)? {
        Some(bytes) => bytes,
        None => return Ok(None),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| PrivateStateError::MalformedCredential)
}

pub(crate) fn write_pairing_answer(
    config_root: &Path,
    confirmed: &str,
) -> Result<(), PrivateStateError> {
    write_pairing_answer_with_fault(config_root, confirmed, &NoWriteFault)
}

pub(crate) fn write_pairing_answer_with_fault(
    config_root: &Path,
    confirmed: &str,
    fault: &dyn DurableWriteFault,
) -> Result<(), PrivateStateError> {
    let answer = PairingAnswer {
        confirmed: confirmed.to_owned(),
    };
    let bytes = serde_json::to_vec(&answer).map_err(|e| PrivateStateError::Io {
        operation: PrivateIoOperation::Persist,
        source: io::Error::new(io::ErrorKind::InvalidData, e),
    })?;
    let path = config_root.join(PAIRING_ANSWER_FILENAME);
    atomic_write_bytes_with_fault(&path, &bytes, fault).map_err(|error| {
        map_private_file(
            error,
            PrivateTargetKind::Credential,
            PrivateIoOperation::Persist,
        )
    })
}

pub(crate) fn is_pairing_confirmed(config_root: &Path, pairing_id: &str) -> bool {
    if pairing_id.is_empty() {
        return false;
    }
    match read_pairing_answer(config_root) {
        Ok(Some(answer)) => !answer.confirmed.is_empty() && answer.confirmed == pairing_id,
        _ => false,
    }
}

pub(crate) fn journal_mark_held_on_disk(config_root: &Path) -> bool {
    let cred = match load_credential(config_root) {
        Ok(Some(cred)) => cred,
        _ => return false,
    };
    let pairing_id = compute_pairing_id(&cred.client_cert_pem);
    match read_pairing_answer(config_root) {
        Ok(Some(answer)) => answer.confirmed != pairing_id,
        Ok(None) => false,
        Err(_) => true,
    }
}

pub(crate) fn grandfather_answer_file(config_root: &Path) -> Result<(), PrivateStateError> {
    let path = config_root.join(PAIRING_ANSWER_FILENAME);
    match open_regular_readonly(&path) {
        Ok(_) => Ok(()),
        Err(PrivateFileError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => {
            let cred = load_credential(config_root)?;
            match cred {
                Some(cred) => {
                    let id = compute_pairing_id(&cred.client_cert_pem);
                    write_pairing_answer(config_root, &id)
                }
                None => write_pairing_answer(config_root, ""),
            }
        }
        Err(_) => Ok(()),
    }
}

pub(crate) fn parse_mark_words(mark: &str) -> Option<[String; 2]> {
    let words = mark
        .split(|c: char| c.is_whitespace() || c == '·' || c == '\u{00B7}')
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    if words.len() == 2 {
        Some([words[0].clone(), words[1].clone()])
    } else {
        None
    }
}

pub(crate) fn compare_mark_to_credential(
    credential: &Credential,
    mark_words: &[String; 2],
) -> Result<bool, ()> {
    let mark = spl_core::mark::mark_from_jid(&credential.instance_id).map_err(|_| ())?;
    let spec = mark.to_render_spec();
    let spec_words = [spec.words[0].to_lowercase(), spec.words[1].to_lowercase()];
    Ok(mark_words == &spec_words)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuestionOutcome {
    Confirmed,
    No,
    Cancel,
    WalkedAway,
}

pub(crate) async fn ask_terminal_question<Fd: AsFd>(
    terminal_fd: Fd,
    credential: &Credential,
) -> QuestionOutcome {
    let spoken = format_spoken_mark(&credential.instance_id);
    let (initial_prompt, ask_line, is_identified) = match spoken {
        Some(spoken) => (
            format!(
                "one more step: check your journal's mark.\n\n  your journal's mark: {spoken}\n\nyour journal shows this same mark in its network app. it should match, exactly.\ndoes this match your journal? type yes or no:\n"
            ),
            "does this match your journal? type yes or no:\n",
            true,
        ),
        None => (
            "one more step: check your journal's mark.\n\n  your journal's mark: unavailable right now\n\ncouldn't verify.\nthis computer couldn't work out your journal's mark, so there's nothing to compare. continue only if you're sure the link came from your journal.\ntype continue to pair anyway, or cancel to stop:\n"
                .to_string(),
            "type continue to pair anyway, or cancel to stop:\n",
            false,
        ),
    };

    let mut tty_file = match terminal_fd.as_fd().try_clone_to_owned() {
        Ok(fd) => File::from(fd),
        Err(_) => return QuestionOutcome::WalkedAway,
    };

    let (mut wake_tx, wake_rx) = match std::os::unix::net::UnixStream::pair() {
        Ok(pair) => pair,
        Err(_) => return QuestionOutcome::WalkedAway,
    };

    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).ok();
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok();

    let blocking_handle = tokio::task::spawn_blocking(move || {
        let mut prompt = initial_prompt.as_str();
        let mut line_buf = Vec::new();
        let mut byte_buf = [0u8; 64];

        loop {
            let _ = tty_file.write_all(prompt.as_bytes());
            let _ = tty_file.flush();
            prompt = ask_line;

            let mut line_complete = false;
            while !line_complete {
                let mut poll_fds = [
                    rustix::event::PollFd::new(&tty_file, rustix::event::PollFlags::IN),
                    rustix::event::PollFd::new(&wake_rx, rustix::event::PollFlags::IN),
                ];

                match rustix::event::poll(&mut poll_fds, None) {
                    Ok(_) => {}
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(_) => return QuestionOutcome::WalkedAway,
                }

                if poll_fds[1].revents().intersects(
                    rustix::event::PollFlags::IN
                        | rustix::event::PollFlags::ERR
                        | rustix::event::PollFlags::HUP,
                ) {
                    return QuestionOutcome::WalkedAway;
                }

                if poll_fds[0]
                    .revents()
                    .intersects(rustix::event::PollFlags::ERR | rustix::event::PollFlags::HUP)
                    && !poll_fds[0].revents().contains(rustix::event::PollFlags::IN)
                {
                    return QuestionOutcome::WalkedAway;
                }

                match tty_file.read(&mut byte_buf) {
                    Ok(0) => return QuestionOutcome::WalkedAway,
                    Ok(n) => {
                        for &b in &byte_buf[..n] {
                            if b == b'\n' {
                                line_complete = true;
                                break;
                            } else if b != b'\r' {
                                line_buf.push(b);
                            }
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => return QuestionOutcome::WalkedAway,
                }
            }

            let text = String::from_utf8_lossy(&line_buf).trim().to_lowercase();
            line_buf.clear();

            if is_identified {
                if text == "yes" || text == "y" {
                    return QuestionOutcome::Confirmed;
                }
                if text == "no" || text == "n" {
                    return QuestionOutcome::No;
                }
            } else {
                if text == "continue" {
                    return QuestionOutcome::Confirmed;
                }
                if text == "cancel" {
                    return QuestionOutcome::Cancel;
                }
            }
        }
    });

    tokio::pin!(blocking_handle);

    let outcome = tokio::select! {
        res = &mut blocking_handle => res.unwrap_or(QuestionOutcome::WalkedAway),
        _ = async {
            if let Some(sig) = &mut sigint {
                sig.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            let _ = wake_tx.write_all(b"w");
            let _ = blocking_handle.await;
            QuestionOutcome::WalkedAway
        }
        _ = async {
            if let Some(sig) = &mut sighup {
                sig.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            let _ = wake_tx.write_all(b"w");
            let _ = blocking_handle.await;
            QuestionOutcome::WalkedAway
        }
    };

    outcome
}

pub(crate) async fn retire_client_registration(credential: &Credential) {
    let Ok(certs) = spl_transport::tls::parse_certs(&credential.client_cert_pem) else {
        return;
    };
    let Some(first_cert) = certs.into_iter().next() else {
        return;
    };
    let hex = spl_core::ca::sha256_hex(first_cert.as_ref());
    let path = format!("/app/network/api/clients/sha256:{hex}");
    let client = if !credential.endpoints.is_empty() {
        spl_transport::TransportClient::new(credential.clone(), None)
    } else {
        spl_transport::TransportClient::new_relay_only(credential.clone(), None)
    };
    let Ok(client) = client else {
        return;
    };
    let _ = tokio::time::timeout(
        SYSTEM_STATUS_TIMEOUT,
        client.request(
            "DELETE",
            &path,
            &[],
            &[],
            spl_transport::RequestOptions::default(),
        ),
    )
    .await;
}

#[cfg(test)]
pub(crate) fn sample_credential(instance_id: &str, cert_pem: &str) -> Credential {
    Credential {
        instance_id: instance_id.to_string(),
        ca_fp_prefix: vec![0x11, 0x22],
        endpoints: vec![],
        local_endpoints: None,
        client_cert_pem: cert_pem.to_string(),
        client_key_pem: String::new(),
        ca_chain_pem: vec![],
        home_label: "home".into(),
        home_attestation: None,
        relay_origin: None,
        device_token: None,
        device_token_expires_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parse_mark_words_variations() {
        assert_eq!(
            parse_mark_words("bramble quokka"),
            Some(["bramble".into(), "quokka".into()])
        );
        assert_eq!(
            parse_mark_words("  BRAMBLE   Quokka  "),
            Some(["bramble".into(), "quokka".into()])
        );
        assert_eq!(
            parse_mark_words("bramble·quokka"),
            Some(["bramble".into(), "quokka".into()])
        );
        assert_eq!(
            parse_mark_words("\tbramble\n\tquokka\r\n"),
            Some(["bramble".into(), "quokka".into()])
        );

        // Hyphen and em-dash are single token -> None
        assert_eq!(parse_mark_words("bramble-quokka"), None);
        assert_eq!(parse_mark_words("bramble—quokka"), None);
        assert_eq!(parse_mark_words(""), None);
        assert_eq!(parse_mark_words("bramble"), None);
        assert_eq!(parse_mark_words("bramble quokka extra"), None);
    }

    #[test]
    fn compare_mark_to_credential_matching() {
        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "pem");
        let mark = spl_core::mark::mark_from_jid(&cred.instance_id).unwrap();
        let spec = mark.to_render_spec();
        let valid_words = [spec.words[0].to_lowercase(), spec.words[1].to_lowercase()];
        assert_eq!(compare_mark_to_credential(&cred, &valid_words), Ok(true));

        let mismatched_words = ["wrong".to_string(), "word".to_string()];
        assert_eq!(
            compare_mark_to_credential(&cred, &mismatched_words),
            Ok(false)
        );

        let unparseable_cred = sample_credential("not-a-uuid", "pem");
        assert_eq!(
            compare_mark_to_credential(&unparseable_cred, &valid_words),
            Err(())
        );
    }

    #[tokio::test]
    async fn answer_lock_mutual_exclusion() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        let lock1 = AnswerLock::acquire(&root).await.unwrap();

        let start = tokio::time::Instant::now();
        let lock2_res = AnswerLock::acquire(&root).await;
        assert!(matches!(
            lock2_res,
            Err(PrivateStateError::Io {
                operation: PrivateIoOperation::Lock,
                ..
            })
        ));
        assert!(start.elapsed() >= Duration::from_millis(1900));

        drop(lock1);
        let lock3 = AnswerLock::acquire(&root).await.unwrap();
        drop(lock3);
    }

    #[test]
    fn answer_file_read_write_and_serde_reject_unknown() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        assert_eq!(read_pairing_answer(&root).unwrap(), None);

        write_pairing_answer(&root, "abc123pairingid").unwrap();
        assert_eq!(
            read_pairing_answer(&root).unwrap(),
            Some(PairingAnswer {
                confirmed: "abc123pairingid".into()
            })
        );

        // Unknown fields rejected
        let path = root.join(PAIRING_ANSWER_FILENAME);
        fs::write(&path, br#"{"confirmed":"abc123pairingid","extra":true}"#).unwrap();
        assert!(matches!(
            read_pairing_answer(&root),
            Err(PrivateStateError::MalformedCredential)
        ));

        // Malformed JSON rejected
        fs::write(&path, br#"not json"#).unwrap();
        assert!(matches!(
            read_pairing_answer(&root),
            Err(PrivateStateError::MalformedCredential)
        ));
    }

    #[test]
    fn grandfather_logic() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        // Absent credential -> writes empty confirmed
        grandfather_answer_file(&root).unwrap();
        assert_eq!(
            read_pairing_answer(&root).unwrap(),
            Some(PairingAnswer {
                confirmed: "".into()
            })
        );

        // Credential present -> writes pairing_id
        let _ = fs::remove_file(root.join(PAIRING_ANSWER_FILENAME));
        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem-bytes");
        crate::private_link::persist_credential(&root, &cred).unwrap();
        let expected_id = compute_pairing_id(&cred.client_cert_pem);

        grandfather_answer_file(&root).unwrap();
        assert_eq!(
            read_pairing_answer(&root).unwrap(),
            Some(PairingAnswer {
                confirmed: expected_id.clone()
            })
        );

        // Does not overwrite an existing answer file
        write_pairing_answer(&root, "custom").unwrap();
        grandfather_answer_file(&root).unwrap();
        assert_eq!(
            read_pairing_answer(&root).unwrap(),
            Some(PairingAnswer {
                confirmed: "custom".into()
            })
        );
    }

    #[test]
    fn held_on_disk_verdict() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        // No credential -> not held
        assert!(!journal_mark_held_on_disk(&root));

        // Credential present, no answer file -> not held (NotFound before grandfather)
        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem-bytes");
        crate::private_link::persist_credential(&root, &cred).unwrap();
        let expected_id = compute_pairing_id(&cred.client_cert_pem);
        assert!(!journal_mark_held_on_disk(&root));

        // Answer empty -> held
        write_pairing_answer(&root, "").unwrap();
        assert!(journal_mark_held_on_disk(&root));

        // Answer mismatch -> held
        write_pairing_answer(&root, "other-id").unwrap();
        assert!(journal_mark_held_on_disk(&root));

        // Answer matching -> NOT held
        write_pairing_answer(&root, &expected_id).unwrap();
        assert!(!journal_mark_held_on_disk(&root));

        // Answer corrupt -> held
        fs::write(root.join(PAIRING_ANSWER_FILENAME), b"bad json").unwrap();
        assert!(journal_mark_held_on_disk(&root));
    }

    #[tokio::test]
    async fn confirm_no_retires_then_drops_the_credential() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let cred = peer.credential();
        crate::private_link::persist_credential(&root, &cred).unwrap();
        write_pairing_answer(&root, "").unwrap();

        let (mut tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        tty_peer.write_all(b"no\n").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let status =
            crate::cli::confirm_async(&root, None, Some(tty_child.as_fd()), &mut out, &mut err)
                .await;

        assert_eq!(status, 1);
        let certs = spl_transport::tls::parse_certs(&cred.client_cert_pem).unwrap();
        let hex = spl_core::ca::sha256_hex(certs[0].as_ref());
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(
            requests[0].path,
            format!("/app/network/api/clients/sha256:{hex}")
        );

        assert!(!root.join("credentials.json").exists());
        assert!(root.join(PAIRING_ANSWER_FILENAME).exists());
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn confirm_mark_mismatch_retires_then_drops_the_credential() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let cred = peer.credential();
        crate::private_link::persist_credential(&root, &cred).unwrap();
        write_pairing_answer(&root, "").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = crate::cli::confirm_async(
            &root,
            Some("wrong words"),
            None::<std::os::fd::BorrowedFd<'_>>,
            &mut out,
            &mut err,
        )
        .await;

        assert_eq!(status, 1);
        let certs = spl_transport::tls::parse_certs(&cred.client_cert_pem).unwrap();
        let hex = spl_core::ca::sha256_hex(certs[0].as_ref());
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(
            requests[0].path,
            format!("/app/network/api/clients/sha256:{hex}")
        );

        assert!(!root.join("credentials.json").exists());
        assert!(root.join(PAIRING_ANSWER_FILENAME).exists());
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn confirm_yes_does_not_confirm_a_replaced_credential() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        let cred_y = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-y");
        let cred_z = sample_credential("89abcdef-0123-4567-89ab-cdef01234567", "cert-z");
        crate::private_link::persist_credential(&root, &cred_y).unwrap();
        write_pairing_answer(&root, "").unwrap();

        let (mut tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        let root_clone = root.clone();
        let cred_z_clone = cred_z.clone();

        let fut1 = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            crate::cli::confirm_async(&root, None, Some(tty_child.as_fd()), &mut out, &mut err)
                .await
        };
        let fut2 = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 64];
            let _ = tty_peer.read(&mut buf).unwrap();
            crate::private_link::persist_credential(&root_clone, &cred_z_clone).unwrap();
            tty_peer.write_all(b"yes\n").unwrap();
        });

        let (status, res2) = tokio::join!(fut1, fut2);
        res2.unwrap();
        assert_eq!(status, 1);
        let answer = read_pairing_answer(&root).unwrap().unwrap();
        let id_z = compute_pairing_id(&cred_z.client_cert_pem);
        assert_ne!(answer.confirmed, id_z);
    }

    #[tokio::test]
    async fn confirm_no_does_not_delete_a_replaced_credential() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        let peer_y = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let cred_y = peer_y.credential();
        let cred_z = sample_credential("89abcdef-0123-4567-89ab-cdef01234567", "cert-z");

        crate::private_link::persist_credential(&root, &cred_y).unwrap();
        write_pairing_answer(&root, "").unwrap();

        let (mut tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        let root_clone = root.clone();
        let cred_z_clone = cred_z.clone();

        let fut1 = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            crate::cli::confirm_async(&root, None, Some(tty_child.as_fd()), &mut out, &mut err)
                .await
        };
        let fut2 = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 64];
            let _ = tty_peer.read(&mut buf).unwrap();
            crate::private_link::persist_credential(&root_clone, &cred_z_clone).unwrap();
            tty_peer.write_all(b"no\n").unwrap();
        });

        let (status, res2) = tokio::join!(fut1, fut2);
        res2.unwrap();
        assert_eq!(status, 1);
        assert!(root.join("credentials.json").exists());
        let current_cred = crate::private_link::load_credential(&root)
            .unwrap()
            .unwrap();
        assert_eq!(current_cred.instance_id, cred_z.instance_id);

        let certs = spl_transport::tls::parse_certs(&cred_y.client_cert_pem).unwrap();
        let hex = spl_core::ca::sha256_hex(certs[0].as_ref());
        let requests = peer_y.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(
            requests[0].path,
            format!("/app/network/api/clients/sha256:{hex}")
        );
        peer_y.shutdown().await;
    }

    #[tokio::test]
    async fn confirm_unverifiable_mark_stays_held() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        let cred = sample_credential("not-a-valid-uuid", "cert-pem");
        crate::private_link::persist_credential(&root, &cred).unwrap();
        write_pairing_answer(&root, "").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = crate::cli::confirm_async(
            &root,
            Some("bramble quokka"),
            None::<std::os::fd::BorrowedFd<'_>>,
            &mut out,
            &mut err,
        )
        .await;

        assert_eq!(status, 5);
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains("couldn't verify."));
        assert!(out_str.contains(CONFIRM_UNVERIFIABLE_LINE));
        assert!(root.join("credentials.json").exists());
    }

    #[tokio::test]
    async fn confirm_without_terminal_or_mark_changes_nothing() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem");
        crate::private_link::persist_credential(&root, &cred).unwrap();
        write_pairing_answer(&root, "").unwrap();

        let cred_bytes_before = fs::read(root.join("credentials.json")).unwrap();
        let answer_bytes_before = fs::read(root.join(PAIRING_ANSWER_FILENAME)).unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = crate::cli::confirm_async(
            &root,
            None,
            None::<std::os::fd::BorrowedFd<'_>>,
            &mut out,
            &mut err,
        )
        .await;

        assert_eq!(status, 1);
        let out_str = String::from_utf8(out).unwrap();
        assert_eq!(out_str, format!("{}\n", CONFIRM_NO_TERMINAL));
        assert_eq!(
            fs::read(root.join("credentials.json")).unwrap(),
            cred_bytes_before
        );
        assert_eq!(
            fs::read(root.join(PAIRING_ANSWER_FILENAME)).unwrap(),
            answer_bytes_before
        );
    }

    #[tokio::test]
    async fn confirm_done_and_unpaired_lines() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = crate::cli::confirm_async(
            &root,
            None,
            None::<std::os::fd::BorrowedFd<'_>>,
            &mut out,
            &mut err,
        )
        .await;
        assert_eq!(status, 1);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{}\n", CONFIRM_UNPAIRED)
        );

        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem");
        crate::private_link::persist_credential(&root, &cred).unwrap();
        let pairing_id = compute_pairing_id(&cred.client_cert_pem);
        write_pairing_answer(&root, &pairing_id).unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = crate::cli::confirm_async(
            &root,
            None,
            None::<std::os::fd::BorrowedFd<'_>>,
            &mut out,
            &mut err,
        )
        .await;
        assert_eq!(status, 0);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{}\n", CONFIRM_DONE)
        );
    }

    #[tokio::test]
    async fn confirm_works_while_app_lock_is_held() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem");
        crate::private_link::persist_credential(&root, &cred).unwrap();
        write_pairing_answer(&root, "").unwrap();

        let mark = spl_core::mark::mark_from_jid(&cred.instance_id).unwrap();
        let spec = mark.to_render_spec();
        let mark_str = format!("{} {}", spec.words[0], spec.words[1]);

        let _app_lock = crate::private_link::PrivateStateLock::acquire(&root).unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = crate::cli::confirm_async(
            &root,
            Some(&mark_str),
            None::<std::os::fd::BorrowedFd<'_>>,
            &mut out,
            &mut err,
        )
        .await;

        assert_eq!(status, 0);
        let id = compute_pairing_id(&cred.client_cert_pem);
        assert_eq!(
            read_pairing_answer(&root).unwrap(),
            Some(PairingAnswer { confirmed: id })
        );
    }

    #[tokio::test]
    async fn mode_000_answer_then_confirm_yes_ends_confirmed() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let temp = tempdir().unwrap();
        let root = temp.path().join("cfg");
        fs::create_dir_all(&root).unwrap();

        let cred = sample_credential("01234567-89ab-cdef-0123-456789abcdef", "cert-pem");
        crate::private_link::persist_credential(&root, &cred).unwrap();

        let ans_path = root.join(PAIRING_ANSWER_FILENAME);
        fs::write(&ans_path, b"initial mode 000 bytes").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ans_path, fs::Permissions::from_mode(0o000)).unwrap();

        let _ = grandfather_answer_file(&root);

        let (mut tty_peer, tty_child) = std::os::unix::net::UnixStream::pair().unwrap();
        tty_peer.write_all(b"yes\n").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let status =
            crate::cli::confirm_async(&root, None, Some(tty_child.as_fd()), &mut out, &mut err)
                .await;

        assert_eq!(status, 0);
        let id = compute_pairing_id(&cred.client_cert_pem);
        let ans = read_pairing_answer(&root).unwrap();
        assert_eq!(ans, Some(PairingAnswer { confirmed: id }));
    }
}
