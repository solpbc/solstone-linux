// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

mod pair_listener {
    use std::{
        collections::HashMap,
        io,
        sync::{Arc, Mutex},
    };

    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
    };
    use rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    };
    use spl_core::{
        PairRequest, PairResponse,
        frame::{
            FLAG_CLOSE, FLAG_DATA, FLAG_OPEN, FLAG_RESET, FLAG_WINDOW, Frame, FrameDecoder,
            RECOMMENDED_CHUNK,
        },
        mux::INITIAL_WINDOW,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::Notify,
        task::JoinHandle,
    };
    use tokio_rustls::{TlsAcceptor, server::TlsStream};

    #[derive(Clone, Debug)]
    #[allow(dead_code)]
    pub(crate) struct RecordedRequest {
        pub(crate) method: String,
        pub(crate) path: String,
        pub(crate) headers: Vec<(String, String)>,
        pub(crate) body: Vec<u8>,
    }

    #[derive(Clone)]
    struct ListenerState {
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        request_arrived: Arc<Notify>,
        issued_client_der: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    pub(crate) struct PairListener {
        link: String,
        state: ListenerState,
        task: JoinHandle<()>,
    }

    impl PairListener {
        pub(crate) async fn start() -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();

            let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
            let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
            ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            ca_params.key_usages = vec![
                KeyUsagePurpose::DigitalSignature,
                KeyUsagePurpose::KeyCertSign,
            ];
            let ca = ca_params.self_signed(&ca_key).unwrap();
            let ca_pem = ca.pem();
            let ca_der = CertificateDer::from(ca.der().to_vec());

            let server_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
            let mut server_params = CertificateParams::new(vec!["spl.local".into()]).unwrap();
            server_params
                .extended_key_usages
                .push(ExtendedKeyUsagePurpose::ServerAuth);
            let server = server_params.signed_by(&server_key, &ca, &ca_key).unwrap();

            let config = ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(server.der().to_vec()), ca_der.clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der())),
            )
            .unwrap();

            let acceptor = TlsAcceptor::from(Arc::new(config));

            let mut blob = vec![0x04, 0x01, 127, 0, 0, 1];
            blob.extend_from_slice(&port.to_be_bytes());
            blob.extend_from_slice(&[0x11; 16]);
            let ca_fp = spl_core::ca::sha256(ca_der.as_ref());
            blob.extend_from_slice(&ca_fp[..16]);
            let link = spl_core::crockford::encode(&blob);

            let state = ListenerState {
                requests: Arc::new(Mutex::new(Vec::new())),
                request_arrived: Arc::new(Notify::new()),
                issued_client_der: Arc::new(Mutex::new(Vec::new())),
            };

            let task_state = state.clone();
            let ca_arc = Arc::new(ca);
            let ca_key_arc = Arc::new(ca_key);

            let task = tokio::spawn(async move {
                let mut carriers = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { break; };
                            let acceptor = acceptor.clone();
                            let state = task_state.clone();
                            let ca = ca_arc.clone();
                            let ca_key = ca_key_arc.clone();
                            let ca_pem = ca_pem.clone();
                            carriers.spawn(async move {
                                if let Ok(tls) = acceptor.accept(stream).await {
                                    let _ = serve_carrier(tls, &state, &ca, &ca_key, &ca_pem).await;
                                }
                            });
                        }
                        _ = carriers.join_next(), if !carriers.is_empty() => {}
                    }
                }
            });

            Self { link, state, task }
        }

        pub(crate) fn link(&self) -> &str {
            &self.link
        }

        pub(crate) fn client_der(&self) -> Vec<u8> {
            self.state
                .issued_client_der
                .lock()
                .unwrap()
                .first()
                .cloned()
                .expect("issued client certificate")
        }

        pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
            self.state.requests.lock().unwrap().clone()
        }

        pub(crate) async fn wait_for_requests(&self, count: usize) {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let notified = self.state.request_arrived.notified();
                    if self.requests().len() >= count {
                        return;
                    }
                    notified.await;
                }
            })
            .await
            .unwrap();
        }

        pub(crate) async fn shutdown(self) {
            self.task.abort();
            let _ = self.task.await;
        }
    }

    struct OutboundResponse {
        bytes: Vec<u8>,
        offset: usize,
        credit: usize,
    }

    async fn serve_carrier(
        mut tls: TlsStream<TcpStream>,
        state: &ListenerState,
        ca: &rcgen::Certificate,
        ca_key: &KeyPair,
        ca_pem: &str,
    ) -> io::Result<()> {
        let mut decoder = FrameDecoder::new();
        let mut requests: HashMap<u32, Vec<u8>> = HashMap::new();
        let mut outbound: HashMap<u32, OutboundResponse> = HashMap::new();
        let mut buffer = [0; 16 * 1024];

        loop {
            let count = tls.read(&mut buffer).await?;
            if count == 0 {
                return Ok(());
            }
            decoder.feed(&buffer[..count]);
            for frame in decoder
                .drain()
                .map_err(|_| io::Error::other("frame decode"))?
            {
                if let Some(pong) = frame.control_pong() {
                    write_frame(&mut tls, pong).await?;
                    continue;
                }
                if frame.flags & FLAG_OPEN != 0 {
                    requests.entry(frame.stream_id).or_default();
                }
                if frame.flags & FLAG_DATA != 0 {
                    let request = requests.entry(frame.stream_id).or_default();
                    request.extend_from_slice(&frame.payload);
                    write_frame(
                        &mut tls,
                        Frame::window(frame.stream_id, frame.payload.len() as u32),
                    )
                    .await?;
                }
                if frame.flags & FLAG_CLOSE != 0 {
                    let raw = requests.remove(&frame.stream_id).unwrap_or_default();
                    if let Some(request) = parse_request(&raw) {
                        state.requests.lock().unwrap().push(request.clone());
                        state.request_arrived.notify_waiters();

                        let response = handle_request(&request, state, ca, ca_key, ca_pem);
                        let mut response = encode_response(response);
                        flush_response(&mut tls, frame.stream_id, &mut response).await?;
                        if response.offset != response.bytes.len() {
                            outbound.insert(frame.stream_id, response);
                        }
                    }
                }
                if frame.flags & FLAG_RESET != 0 {
                    requests.remove(&frame.stream_id);
                }
                if frame.flags & FLAG_WINDOW != 0
                    && let (Some(credit), Some(response)) =
                        (frame.window_credit(), outbound.get_mut(&frame.stream_id))
                {
                    response.credit = response.credit.saturating_add(credit as usize);
                    flush_response(&mut tls, frame.stream_id, response).await?;
                    if response.offset == response.bytes.len() {
                        outbound.remove(&frame.stream_id);
                    }
                }
            }
        }
    }

    struct Response {
        status: u16,
        body: Vec<u8>,
    }

    fn handle_request(
        request: &RecordedRequest,
        state: &ListenerState,
        ca: &rcgen::Certificate,
        ca_key: &KeyPair,
        ca_pem: &str,
    ) -> Response {
        if request.method == "POST" && request.path.starts_with("/app/network/pair") {
            let pair_req: PairRequest = match serde_json::from_slice(&request.body) {
                Ok(r) => r,
                Err(_) => {
                    return Response {
                        status: 400,
                        body: Vec::new(),
                    };
                }
            };
            let csr_params = match rcgen::CertificateSigningRequestParams::from_pem(&pair_req.csr) {
                Ok(p) => p,
                Err(_) => {
                    return Response {
                        status: 400,
                        body: Vec::new(),
                    };
                }
            };
            let client_cert = match csr_params.signed_by(ca, ca_key) {
                Ok(c) => c,
                Err(_) => {
                    return Response {
                        status: 500,
                        body: Vec::new(),
                    };
                }
            };
            let client_der = client_cert.der().to_vec();
            state
                .issued_client_der
                .lock()
                .unwrap()
                .push(client_der.clone());

            let pair_resp = PairResponse {
                client_cert: client_cert.pem(),
                ca_chain: vec![ca_pem.to_owned()],
                instance_id: "01234567-89ab-cdef-0123-456789abcdef".to_string(),
                home_label: "test home".to_string(),
                fingerprint: format!("sha256:{}", spl_core::ca::sha256_hex(&client_der)),
                home_attestation: None,
                local_endpoints: None,
                relay_access: None,
            };
            let body = serde_json::to_vec(&pair_resp).unwrap();
            Response { status: 200, body }
        } else if request.method == "DELETE" {
            Response {
                status: 200,
                body: Vec::new(),
            }
        } else {
            Response {
                status: 404,
                body: Vec::new(),
            }
        }
    }

    async fn write_frame(tls: &mut TlsStream<TcpStream>, frame: Frame) -> io::Result<()> {
        tls.write_all(
            &frame
                .encode()
                .map_err(|_| io::Error::other("frame encode"))?,
        )
        .await
    }

    fn encode_response(response: Response) -> OutboundResponse {
        let mut head = format!("HTTP/1.1 {} OK\r\n", response.status);
        head.push_str(&format!("content-length: {}\r\n\r\n", response.body.len()));
        let mut bytes = head.into_bytes();
        bytes.extend(response.body);
        OutboundResponse {
            bytes,
            offset: 0,
            credit: INITIAL_WINDOW,
        }
    }

    async fn flush_response(
        tls: &mut TlsStream<TcpStream>,
        stream: u32,
        response: &mut OutboundResponse,
    ) -> io::Result<()> {
        while response.offset < response.bytes.len() && response.credit > 0 {
            let count = (response.bytes.len() - response.offset)
                .min(RECOMMENDED_CHUNK)
                .min(response.credit);
            let end = response.offset + count;
            let last = end == response.bytes.len();
            write_frame(
                tls,
                Frame::new(
                    stream,
                    FLAG_DATA | if last { FLAG_CLOSE } else { 0 },
                    response.bytes[response.offset..end].to_vec(),
                ),
            )
            .await?;
            response.offset = end;
            response.credit -= count;
        }
        Ok(())
    }

    fn parse_request(raw: &[u8]) -> Option<RecordedRequest> {
        let split = raw.windows(4).position(|part| part == b"\r\n\r\n")?;
        let head = std::str::from_utf8(&raw[..split]).ok()?;
        let mut lines = head.split("\r\n");
        let mut request = lines.next()?.split_whitespace();
        let method = request.next()?.to_owned();
        let path = request.next()?.to_owned();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_owned(), value.trim().to_owned()))
            .collect();
        Some(RecordedRequest {
            method,
            path,
            headers,
            body: raw[split + 4..].to_vec(),
        })
    }
}

use pair_listener::PairListener;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

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
    let fifo = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo_path)
        .unwrap();
    let mut reader = BufReader::new(fifo);
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
    let fifo = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo_path)
        .unwrap();
    let mut reader = BufReader::new(fifo);
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

#[tokio::test]
async fn setup_prompt_traps_sigint_and_exits_5_with_held_guidance() {
    let bin = env!("CARGO_BIN_EXE_solstone-linux");
    let temp = tempdir().unwrap();
    let xdg_config = temp.path().join("config");
    let xdg_data = temp.path().join("data");
    let app_config = xdg_config.join("solstone-linux");
    let app_data = xdg_data.join("solstone-linux");
    fs::create_dir_all(&app_config).unwrap();
    fs::create_dir_all(&app_data).unwrap();

    let peer = PairListener::start().await;
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
        stdin.write_all(peer.link().as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }

    let fifo = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo_path)
        .await
        .unwrap();
    let mut reader = tokio::io::BufReader::new(fifo);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while reader.read_line(&mut line).await.unwrap() > 0 {
            if line.contains("does this match your journal?") {
                break;
            }
            line.clear();
        }
    })
    .await
    .unwrap();

    drop(child.stdout.take());
    drop(child.stderr.take());

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
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(status.code(), Some(5));
    let cred_path = app_config.join("credentials.json");
    assert!(cred_path.exists());
    let ans_path = app_config.join("pairing-answer.json");
    assert!(ans_path.exists());
    let ans_content = fs::read_to_string(&ans_path).unwrap();
    assert!(ans_content.contains(r#""confirmed":"""#));
    peer.shutdown().await;
}

#[tokio::test]
async fn setup_prompt_traps_sighup_and_exits_5_with_held_guidance() {
    let bin = env!("CARGO_BIN_EXE_solstone-linux");
    let temp = tempdir().unwrap();
    let xdg_config = temp.path().join("config");
    let xdg_data = temp.path().join("data");
    let app_config = xdg_config.join("solstone-linux");
    let app_data = xdg_data.join("solstone-linux");
    fs::create_dir_all(&app_config).unwrap();
    fs::create_dir_all(&app_data).unwrap();

    let peer = PairListener::start().await;
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
        stdin.write_all(peer.link().as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }

    let fifo = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo_path)
        .await
        .unwrap();
    let mut reader = tokio::io::BufReader::new(fifo);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while reader.read_line(&mut line).await.unwrap() > 0 {
            if line.contains("does this match your journal?") {
                break;
            }
            line.clear();
        }
    })
    .await
    .unwrap();

    drop(child.stdout.take());
    drop(child.stderr.take());

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
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(status.code(), Some(5));
    let cred_path = app_config.join("credentials.json");
    assert!(cred_path.exists());
    let ans_path = app_config.join("pairing-answer.json");
    assert!(ans_path.exists());
    let ans_content = fs::read_to_string(&ans_path).unwrap();
    assert!(ans_content.contains(r#""confirmed":"""#));
    peer.shutdown().await;
}

#[tokio::test]
async fn setup_prompt_pty_read_error_exits_5_with_held_guidance() {
    let bin = env!("CARGO_BIN_EXE_solstone-linux");
    let temp = tempdir().unwrap();
    let xdg_config = temp.path().join("config");
    let xdg_data = temp.path().join("data");
    let app_config = xdg_config.join("solstone-linux");
    let app_data = xdg_data.join("solstone-linux");
    fs::create_dir_all(&app_config).unwrap();
    fs::create_dir_all(&app_data).unwrap();

    let tty_path = temp.path().join("mark_tty.txt");
    fs::write(&tty_path, b"").unwrap();

    let peer = PairListener::start().await;

    let mut child = Command::new(bin)
        .arg("setup")
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("SOLSTONE_LINUX_MARK_TTY", &tty_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(peer.link().as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }

    drop(child.stdout.take());
    drop(child.stderr.take());

    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("child hung and did not exit within 5s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    let content = fs::read_to_string(&tty_path).unwrap();
    assert!(content.contains("does this match your journal?"));

    assert_eq!(status.code(), Some(5));
    let cred_path = app_config.join("credentials.json");
    assert!(cred_path.exists());
    let ans_path = app_config.join("pairing-answer.json");
    assert!(ans_path.exists());
    let ans_content = fs::read_to_string(&ans_path).unwrap();
    assert!(ans_content.contains(r#""confirmed":"""#));
    peer.shutdown().await;
}

#[tokio::test]
async fn setup_prompt_when_x_already_confirmed_re_pair_terminal_no_exits_1() {
    let bin = env!("CARGO_BIN_EXE_solstone-linux");
    let temp = tempdir().unwrap();
    let xdg_config = temp.path().join("config");
    let xdg_data = temp.path().join("data");
    let app_config = xdg_config.join("solstone-linux");
    let app_data = xdg_data.join("solstone-linux");
    fs::create_dir_all(&app_config).unwrap();
    fs::create_dir_all(&app_data).unwrap();

    let cred_x = generate_test_credential();
    let cred_x_bytes = serde_json::to_vec(&cred_x).unwrap();
    let id_x = spl_core::ca::sha256_hex(&cred_x.ca_fp_prefix);
    let cred_x_path = app_config.join("credentials.json");
    fs::write(&cred_x_path, &cred_x_bytes).unwrap();
    fs::set_permissions(&cred_x_path, fs::Permissions::from_mode(0o600)).unwrap();

    let ans_path = app_config.join("pairing-answer.json");
    fs::write(&ans_path, format!(r#"{{"confirmed":"{id_x}"}}"#)).unwrap();
    fs::set_permissions(&ans_path, fs::Permissions::from_mode(0o600)).unwrap();

    let fifo_path = temp.path().join("mark_tty.fifo");
    create_fifo(&fifo_path);

    let peer_y = PairListener::start().await;

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
        stdin.write_all(peer_y.link().as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }

    let fifo = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo_path)
        .await
        .unwrap();
    let (read_half, mut write_half) = tokio::io::split(fifo);
    let mut reader = tokio::io::BufReader::new(read_half);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while reader.read_line(&mut line).await.unwrap() > 0 {
            if line.contains("does this match your journal?") {
                break;
            }
            line.clear();
        }
    })
    .await
    .unwrap();

    write_half.write_all(b"no\n").await.unwrap();

    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("child hung and did not exit within 5s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(status.code(), Some(1));
    assert_eq!(fs::read(&cred_x_path).unwrap(), cred_x_bytes);
    let ans_content = fs::read_to_string(&ans_path).unwrap();
    assert_eq!(ans_content, format!(r#"{{"confirmed":"{id_x}"}}"#));

    peer_y.wait_for_requests(2).await;
    let requests = peer_y.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "DELETE");
    let hex_y = spl_core::ca::sha256_hex(&peer_y.client_der());
    assert_eq!(
        requests[1].path,
        format!("/app/network/api/clients/sha256:{hex_y}")
    );
    peer_y.shutdown().await;
}

#[tokio::test]
async fn setup_prompt_repeats_ask_line_on_empty_or_unknown_word() {
    let bin = env!("CARGO_BIN_EXE_solstone-linux");
    let temp = tempdir().unwrap();
    let xdg_config = temp.path().join("config");
    let xdg_data = temp.path().join("data");
    let app_config = xdg_config.join("solstone-linux");
    let app_data = xdg_data.join("solstone-linux");
    fs::create_dir_all(&app_config).unwrap();
    fs::create_dir_all(&app_data).unwrap();

    let fifo_path = temp.path().join("mark_tty.fifo");
    create_fifo(&fifo_path);

    let peer = PairListener::start().await;

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
        stdin.write_all(peer.link().as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }

    let fifo = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo_path)
        .await
        .unwrap();
    let (read_half, mut write_half) = tokio::io::split(fifo);
    let mut reader = tokio::io::BufReader::new(read_half);
    let mut line = String::new();

    // 1. Wait for first prompt
    tokio::time::timeout(Duration::from_secs(5), async {
        while reader.read_line(&mut line).await.unwrap() > 0 {
            if line.contains("does this match your journal?") {
                break;
            }
            line.clear();
        }
    })
    .await
    .unwrap();

    // Send empty line
    write_half.write_all(b"\n").await.unwrap();

    // 2. Wait for second prompt
    line.clear();
    tokio::time::timeout(Duration::from_secs(5), async {
        while reader.read_line(&mut line).await.unwrap() > 0 {
            if line.contains("does this match your journal?") {
                break;
            }
            line.clear();
        }
    })
    .await
    .unwrap();

    // Send unknown word
    write_half.write_all(b"banana\n").await.unwrap();

    // 3. Wait for third prompt
    line.clear();
    tokio::time::timeout(Duration::from_secs(5), async {
        while reader.read_line(&mut line).await.unwrap() > 0 {
            if line.contains("does this match your journal?") {
                break;
            }
            line.clear();
        }
    })
    .await
    .unwrap();

    // Send yes
    write_half.write_all(b"yes\n").await.unwrap();

    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("child hung and did not exit within 5s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(status.code(), Some(0));
    let ans_path = app_config.join("pairing-answer.json");
    let ans_content = fs::read_to_string(&ans_path).unwrap();
    assert!(!ans_content.contains(r#""confirmed":"""#));
    peer.shutdown().await;
}
