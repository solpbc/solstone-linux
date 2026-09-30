// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::tempdir;

fn generate_test_credential() -> spl_transport::credential::Credential {
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
    };
    use rustls::pki_types::CertificateDer;

    let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
    ];
    let ca = ca_params.self_signed(&ca_key).unwrap();

    let client_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let mut client_params = CertificateParams::new(vec!["observer.test".into()]).unwrap();
    client_params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ClientAuth);
    let client = client_params.signed_by(&client_key, &ca, &ca_key).unwrap();
    let ca_der = CertificateDer::from(ca.der().to_vec());

    spl_transport::credential::Credential {
        client_key_pem: client_key.serialize_pem(),
        client_cert_pem: client.pem(),
        ca_chain_pem: vec![ca.pem()],
        ca_fp_prefix: spl_core::ca::sha256(ca_der.as_ref())[..16].to_vec(),
        instance_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
        home_label: "test home".into(),
        endpoints: vec![],
        local_endpoints: None,
        relay_origin: None,
        device_token: None,
        device_token_expires_at: None,
        home_attestation: None,
    }
}

fn create_fifo(path: &std::path::Path) {
    let status = Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("mkfifo must succeed");
    assert!(status.success());
}

#[test]
fn confirm_prompt_traps_sigint_and_exits_5_with_held_guidance() {
    let bin = env!("CARGO_BIN_EXE_solstone-linux");
    let temp = tempdir().unwrap();
    let xdg_config = temp.path().join("config");
    let xdg_data = temp.path().join("data");
    let app_config = xdg_config.join("solstone-linux");
    let app_data = xdg_data.join("solstone-linux");
    fs::create_dir_all(&app_config).unwrap();
    fs::create_dir_all(&app_data).unwrap();

    let cred = generate_test_credential();
    let cred_path = app_config.join("credentials.json");
    fs::write(&cred_path, serde_json::to_vec(&cred).unwrap()).unwrap();
    fs::set_permissions(&cred_path, fs::Permissions::from_mode(0o600)).unwrap();

    let ans_path = app_config.join("pairing-answer.json");
    fs::write(&ans_path, br#"{"confirmed":""}"#).unwrap();
    fs::set_permissions(&ans_path, fs::Permissions::from_mode(0o600)).unwrap();

    let fifo_path = temp.path().join("mark_tty.fifo");
    create_fifo(&fifo_path);

    let mut child = Command::new(bin)
        .arg("confirm")
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("SOLSTONE_LINUX_MARK_TTY", &fifo_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    drop(child.stdout.take());
    drop(child.stderr.take());

    use std::io::BufRead;
    let fifo = fs::File::open(&fifo_path).unwrap();
    let mut reader = std::io::BufReader::new(fifo);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap() > 0 {
        if line.contains("does this match your journal?") {
            break;
        }
        line.clear();
    }

    let _ = rustix::process::kill_process(
        rustix::process::Pid::from_raw(child.id() as i32).unwrap(),
        rustix::process::Signal::INT,
    );

    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("child hung and did not exit within 5s");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    assert_eq!(status.code(), Some(5));
    assert!(cred_path.exists());
    let answer_bytes = fs::read_to_string(&ans_path).unwrap();
    assert!(answer_bytes.contains(r#""confirmed":"""#));
}

#[test]
fn confirm_prompt_traps_sighup_and_exits_5_with_held_guidance() {
    let bin = env!("CARGO_BIN_EXE_solstone-linux");
    let temp = tempdir().unwrap();
    let xdg_config = temp.path().join("config");
    let xdg_data = temp.path().join("data");
    let app_config = xdg_config.join("solstone-linux");
    let app_data = xdg_data.join("solstone-linux");
    fs::create_dir_all(&app_config).unwrap();
    fs::create_dir_all(&app_data).unwrap();

    let cred = generate_test_credential();
    let cred_path = app_config.join("credentials.json");
    fs::write(&cred_path, serde_json::to_vec(&cred).unwrap()).unwrap();
    fs::set_permissions(&cred_path, fs::Permissions::from_mode(0o600)).unwrap();

    let ans_path = app_config.join("pairing-answer.json");
    fs::write(&ans_path, br#"{"confirmed":""}"#).unwrap();
    fs::set_permissions(&ans_path, fs::Permissions::from_mode(0o600)).unwrap();

    let fifo_path = temp.path().join("mark_tty.fifo");
    create_fifo(&fifo_path);

    let mut child = Command::new(bin)
        .arg("confirm")
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("SOLSTONE_LINUX_MARK_TTY", &fifo_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    drop(child.stdout.take());
    drop(child.stderr.take());

    use std::io::BufRead;
    let fifo = fs::File::open(&fifo_path).unwrap();
    let mut reader = std::io::BufReader::new(fifo);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap() > 0 {
        if line.contains("does this match your journal?") {
            break;
        }
        line.clear();
    }

    let _ = rustix::process::kill_process(
        rustix::process::Pid::from_raw(child.id() as i32).unwrap(),
        rustix::process::Signal::HUP,
    );

    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("child hung and did not exit within 5s");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    assert_eq!(status.code(), Some(5));
    assert!(cred_path.exists());
    let answer_bytes = fs::read_to_string(&ans_path).unwrap();
    assert!(answer_bytes.contains(r#""confirmed":"""#));
}

#[test]
fn setup_ceremony_sigint_kills_process_and_saves_nothing() {
    let bin = env!("CARGO_BIN_EXE_solstone-linux");
    let temp = tempdir().unwrap();
    let xdg_config = temp.path().join("config");
    let xdg_data = temp.path().join("data");
    let app_config = xdg_config.join("solstone-linux");
    let app_data = xdg_data.join("solstone-linux");
    fs::create_dir_all(&app_config).unwrap();
    fs::create_dir_all(&app_data).unwrap();

    let config_path = app_config.join("config.json");
    let initial_config = br#"{"version":1}"#;
    fs::write(&config_path, initial_config).unwrap();

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();

    let mut blob = vec![0x04, 0x01, 127, 0, 0, 1];
    blob.extend_from_slice(&port.to_be_bytes());
    blob.extend_from_slice(&[0x11; 16]);
    blob.extend_from_slice(&[0x22; 16]);
    let link = spl_core::crockford::encode(&blob);

    let fifo_path = temp.path().join("mark_tty.fifo");
    create_fifo(&fifo_path);

    let mut child = Command::new(bin)
        .arg("setup")
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("SOLSTONE_LINUX_MARK_TTY", &fifo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(link.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }

    // Accept TCP connection, then send SIGINT before question bytes
    let (_sock, _) = listener.accept().unwrap();

    let _ = rustix::process::kill_process(
        rustix::process::Pid::from_raw(child.id() as i32).unwrap(),
        rustix::process::Signal::INT,
    );

    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("child hung and did not exit within 5s");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(2));

    let cred_path = app_config.join("credentials.json");
    assert!(!cred_path.exists());
    let current_config = fs::read(&config_path).unwrap();
    assert_eq!(current_config, initial_config);
}
