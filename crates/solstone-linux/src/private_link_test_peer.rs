// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    PKCS_ECDSA_P256_SHA256,
};
use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    server::WebPkiClientVerifier,
};
use spl_core::{
    frame::{
        FLAG_CLOSE, FLAG_DATA, FLAG_OPEN, FLAG_RESET, FLAG_WINDOW, Frame, FrameDecoder,
        RECOMMENDED_CHUNK,
    },
    mux::INITIAL_WINDOW,
};
use spl_transport::credential::{Credential, EndpointAddr};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

#[derive(Clone)]
pub(crate) struct PeerRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

#[derive(Clone)]
struct PeerResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    gate: Option<Arc<Notify>>,
    nonblocking_gate: Option<Arc<std::sync::atomic::AtomicBool>>,
}

struct OutboundResponse {
    bytes: Vec<u8>,
    offset: usize,
    credit: usize,
}

#[derive(Clone)]
struct PeerState {
    responses: Arc<Mutex<VecDeque<PeerResponse>>>,
    requests: Arc<Mutex<Vec<PeerRequest>>>,
    request_arrived: Arc<Notify>,
    accepted: Arc<AtomicUsize>,
    response_gate_changed: Arc<Notify>,
    hold_request_credit: Arc<std::sync::atomic::AtomicBool>,
    request_credit_changed: Arc<Notify>,
    max_request_staged: Arc<AtomicUsize>,
    request_staged_changed: Arc<Notify>,
}

pub(crate) struct PrivateLinkPeer {
    credential: Credential,
    state: PeerState,
    task: JoinHandle<()>,
}

impl PrivateLinkPeer {
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (credential, acceptor) = credential_and_acceptor(listener.local_addr().unwrap().port());
        let state = PeerState {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            request_arrived: Arc::new(Notify::new()),
            accepted: Arc::new(AtomicUsize::new(0)),
            response_gate_changed: Arc::new(Notify::new()),
            hold_request_credit: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            request_credit_changed: Arc::new(Notify::new()),
            max_request_staged: Arc::new(AtomicUsize::new(0)),
            request_staged_changed: Arc::new(Notify::new()),
        };
        let task_state = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                task_state.accepted.fetch_add(1, Ordering::SeqCst);
                let Ok(tls) = acceptor.accept(stream).await else {
                    continue;
                };
                let _ = serve_carrier(tls, &task_state).await;
            }
        });
        Self {
            credential,
            state,
            task,
        }
    }

    pub(crate) fn credential(&self) -> Credential {
        self.credential.clone()
    }
    pub(crate) fn enqueue_response(&self, status: u16, body: impl Into<Vec<u8>>) {
        self.state
            .responses
            .lock()
            .unwrap()
            .push_back(PeerResponse {
                status,
                headers: Vec::new(),
                body: body.into(),
                gate: None,
                nonblocking_gate: None,
            });
    }
    pub(crate) fn enqueue_response_with_headers(
        &self,
        status: u16,
        headers: Vec<(String, String)>,
        body: impl Into<Vec<u8>>,
    ) {
        self.state
            .responses
            .lock()
            .unwrap()
            .push_back(PeerResponse {
                status,
                headers,
                body: body.into(),
                gate: None,
                nonblocking_gate: None,
            });
    }
    pub(crate) fn enqueue_gated_response(
        &self,
        status: u16,
        body: impl Into<Vec<u8>>,
        gate: Arc<Notify>,
    ) {
        self.state
            .responses
            .lock()
            .unwrap()
            .push_back(PeerResponse {
                status,
                headers: Vec::new(),
                body: body.into(),
                gate: Some(gate),
                nonblocking_gate: None,
            });
    }
    pub(crate) fn gate_next_response_nonblocking(&self, gate: Arc<std::sync::atomic::AtomicBool>) {
        self.state
            .responses
            .lock()
            .unwrap()
            .front_mut()
            .expect("response to gate")
            .nonblocking_gate = Some(gate);
    }
    pub(crate) fn gate_queued_responses_nonblocking(
        &self,
        count: usize,
        gate: Arc<std::sync::atomic::AtomicBool>,
    ) {
        for response in self.state.responses.lock().unwrap().iter_mut().take(count) {
            response.nonblocking_gate = Some(gate.clone());
        }
    }
    pub(crate) fn gate_queued_response_nonblocking(
        &self,
        index: usize,
        gate: Arc<std::sync::atomic::AtomicBool>,
    ) {
        self.state
            .responses
            .lock()
            .unwrap()
            .get_mut(index)
            .expect("response to gate")
            .nonblocking_gate = Some(gate);
    }
    pub(crate) fn notify_response_gates(&self) {
        self.state.response_gate_changed.notify_waiters();
    }
    pub(crate) fn hold_request_credit(&self) {
        self.state
            .hold_request_credit
            .store(true, Ordering::Release);
    }
    pub(crate) fn release_request_credit(&self) {
        self.state
            .hold_request_credit
            .store(false, Ordering::Release);
        self.state.request_credit_changed.notify_waiters();
    }
    pub(crate) fn max_request_staged(&self) -> usize {
        self.state.max_request_staged.load(Ordering::Acquire)
    }
    pub(crate) async fn wait_for_request_staged_at_least(&self, count: usize) {
        loop {
            let notified = self.state.request_staged_changed.notified();
            if self.max_request_staged() >= count {
                return;
            }
            notified.await;
        }
    }
    pub(crate) fn requests(&self) -> Vec<PeerRequest> {
        self.state.requests.lock().unwrap().clone()
    }
    pub(crate) fn accepted_carriers(&self) -> usize {
        self.state.accepted.load(Ordering::SeqCst)
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

fn credential_and_acceptor(port: u16) -> (Credential, TlsAcceptor) {
    let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
    ];
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let server_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let mut server_params = CertificateParams::new(vec!["spl.local".into()]).unwrap();
    server_params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ServerAuth);
    let server = server_params.signed_by(&server_key, &ca, &ca_key).unwrap();
    let client_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let mut client_params = CertificateParams::new(vec!["observer.test".into()]).unwrap();
    client_params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ClientAuth);
    let client = client_params.signed_by(&client_key, &ca, &ca_key).unwrap();
    let ca_der = CertificateDer::from(ca.der().to_vec());
    let mut roots = RootCertStore::empty();
    roots.add(ca_der.clone()).unwrap();
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .unwrap();
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![CertificateDer::from(server.der().to_vec()), ca_der.clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der())),
            )
            .unwrap();
    (
        Credential {
            client_key_pem: client_key.serialize_pem(),
            client_cert_pem: client.pem(),
            ca_chain_pem: vec![ca.pem()],
            ca_fp_prefix: spl_core::ca::sha256(ca_der.as_ref())[..16].to_vec(),
            instance_id: "test-instance".into(),
            home_label: "test home".into(),
            endpoints: vec![EndpointAddr {
                host: "127.0.0.1".into(),
                port,
            }],
            home_attestation: None,
            local_endpoints: None,
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        },
        TlsAcceptor::from(Arc::new(config)),
    )
}

async fn serve_carrier(mut tls: TlsStream<TcpStream>, state: &PeerState) -> io::Result<()> {
    let mut decoder = FrameDecoder::new();
    let mut requests: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut outbound: HashMap<u32, OutboundResponse> = HashMap::new();
    let mut gated: HashMap<u32, PeerResponse> = HashMap::new();
    let mut pending_request_credit: HashMap<u32, usize> = HashMap::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = tokio::select! {
            count = tls.read(&mut buffer) => count?,
            () = state.response_gate_changed.notified() => {
                let ready = gated
                    .iter()
                    .filter(|(_, response)| {
                        response
                            .nonblocking_gate
                            .as_ref()
                            .is_none_or(|gate| gate.load(Ordering::Acquire))
                    })
                    .map(|(stream, _)| *stream)
                    .collect::<Vec<_>>();
                for stream in ready {
                    let mut response = encode_response(gated.remove(&stream).unwrap());
                    flush_response(&mut tls, stream, &mut response).await?;
                    if response.offset != response.bytes.len() {
                        outbound.insert(stream, response);
                    }
                }
                continue;
            }
            () = state.request_credit_changed.notified() => {
                if !state.hold_request_credit.load(Ordering::Acquire) {
                    for (stream, credit) in pending_request_credit.drain() {
                        write_frame(&mut tls, Frame::window(stream, credit as u32)).await?;
                    }
                }
                continue;
            }
        };
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
                state
                    .max_request_staged
                    .fetch_max(request.len(), Ordering::AcqRel);
                state.request_staged_changed.notify_waiters();
                if state.hold_request_credit.load(Ordering::Acquire) {
                    *pending_request_credit.entry(frame.stream_id).or_default() +=
                        frame.payload.len();
                } else {
                    write_frame(
                        &mut tls,
                        Frame::window(frame.stream_id, frame.payload.len() as u32),
                    )
                    .await?;
                }
            }
            if frame.flags & FLAG_CLOSE != 0 {
                let raw = requests.remove(&frame.stream_id).unwrap_or_default();
                if let Some(request) = parse_request(&raw) {
                    state.requests.lock().unwrap().push(request);
                    state.request_arrived.notify_waiters();
                }
                let response =
                    state
                        .responses
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or(PeerResponse {
                            status: 500,
                            headers: Vec::new(),
                            body: Vec::new(),
                            gate: None,
                            nonblocking_gate: None,
                        });
                if let Some(gate) = &response.gate {
                    gate.notified().await;
                }
                if response
                    .nonblocking_gate
                    .as_ref()
                    .is_some_and(|gate| !gate.load(Ordering::Acquire))
                {
                    gated.insert(frame.stream_id, response);
                    continue;
                }
                let mut response = encode_response(response);
                flush_response(&mut tls, frame.stream_id, &mut response).await?;
                if response.offset != response.bytes.len() {
                    outbound.insert(frame.stream_id, response);
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

async fn write_frame(tls: &mut TlsStream<TcpStream>, frame: Frame) -> io::Result<()> {
    tls.write_all(
        &frame
            .encode()
            .map_err(|_| io::Error::other("frame encode"))?,
    )
    .await
}

fn encode_response(response: PeerResponse) -> OutboundResponse {
    let mut head = format!("HTTP/1.1 {} OK\r\n", response.status);
    for (name, value) in response.headers {
        head.push_str(&name);
        head.push_str(": ");
        head.push_str(&value);
        head.push_str("\r\n");
    }
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

fn parse_request(raw: &[u8]) -> Option<PeerRequest> {
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
    Some(PeerRequest {
        method,
        path,
        headers,
        body: raw[split + 4..].to_vec(),
    })
}
