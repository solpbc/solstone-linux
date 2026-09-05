// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    observer::Clock,
    private_link::{PrivateLinkCapability, PrivateLinkSession, start_private_link_session},
    private_link_test_peer::PrivateLinkPeer,
};
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response,
    body::{Body, Bytes, Frame, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    future::Future,
    io,
    net::TcpListener as StdTcpListener,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc},
    task::JoinHandle,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub(crate) const PROGRESS_BOUND: Duration = Duration::from_secs(30);

struct RestorePausedClock;

impl Drop for RestorePausedClock {
    fn drop(&mut self) {
        tokio::time::pause();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DayCustodyLeg {
    Manifest,
    DayManifest,
    Segments,
}

#[derive(Clone, Debug)]
pub(crate) struct DayCustodyFixture {
    day: String,
    items: Vec<Value>,
    absent: bool,
    day_manifest_day: Option<String>,
    version: u64,
    protocol_version: u64,
    total: Option<u64>,
    malformed: Option<(DayCustodyLeg, Vec<u8>)>,
    failed: Option<(DayCustodyLeg, u16, Vec<u8>)>,
}

impl DayCustodyFixture {
    pub(crate) fn new(day: impl Into<String>, items: Vec<Value>) -> Self {
        let day = day.into();
        Self {
            day,
            items,
            absent: false,
            day_manifest_day: None,
            version: 1,
            protocol_version: 3,
            total: None,
            malformed: None,
            failed: None,
        }
    }

    pub(crate) fn absent(day: impl Into<String>) -> Self {
        let mut fixture = Self::new(day, Vec::new());
        fixture.absent = true;
        fixture
    }

    pub(crate) fn with_day_manifest_day(mut self, day: impl Into<String>) -> Self {
        self.day_manifest_day = Some(day.into());
        self
    }

    pub(crate) fn with_version(mut self, version: u64) -> Self {
        self.version = version;
        self
    }

    pub(crate) fn with_segments_protocol_version(mut self, version: u64) -> Self {
        self.protocol_version = version;
        self
    }

    pub(crate) fn with_segments_total(mut self, total: u64) -> Self {
        self.total = Some(total);
        self
    }

    pub(crate) fn with_malformed_leg(
        mut self,
        leg: DayCustodyLeg,
        body: impl Into<Vec<u8>>,
    ) -> Self {
        self.malformed = Some((leg, body.into()));
        self
    }

    pub(crate) fn with_http_failure(
        mut self,
        leg: DayCustodyLeg,
        status: u16,
        body: impl Into<Vec<u8>>,
    ) -> Self {
        self.failed = Some((leg, status, body.into()));
        self
    }

    pub(crate) fn response_for(&self, leg: DayCustodyLeg) -> (u16, Vec<u8>) {
        if let Some((failed_leg, status, bytes)) = &self.failed
            && *failed_leg == leg
        {
            return (*status, bytes.clone());
        }
        if let Some((malformed_leg, bytes)) = &self.malformed
            && *malformed_leg == leg
        {
            return (200, bytes.clone());
        }
        let body = match leg {
            DayCustodyLeg::Manifest => {
                let mut days = serde_json::Map::new();
                if !self.absent {
                    days.insert(
                        self.day.clone(),
                        serde_json::json!({"segments": self.items.len()}),
                    );
                }
                serde_json::json!({"days": days})
            }
            DayCustodyLeg::DayManifest => serde_json::json!({
                "day": self.day_manifest_day.as_deref().unwrap_or(&self.day),
                "version": self.version,
                "segments": {},
            }),
            DayCustodyLeg::Segments => serde_json::json!({
                "protocol_version": self.protocol_version,
                "total": self.total.unwrap_or(self.items.len() as u64),
                "items": self.items,
            }),
        };
        (200, body.to_string().into_bytes())
    }

    pub(crate) fn stops_after(&self, leg: DayCustodyLeg) -> bool {
        (self.absent && leg == DayCustodyLeg::Manifest)
            || self
                .failed
                .as_ref()
                .is_some_and(|(failed_leg, _, _)| *failed_leg == leg)
            || self
                .malformed
                .as_ref()
                .is_some_and(|(malformed_leg, _)| *malformed_leg == leg)
    }
}

pub(crate) fn day_custody_fixture(value: &Value) -> Option<DayCustodyFixture> {
    if let Some(items) = value.get("day_custody_items").and_then(Value::as_array) {
        let day = value
            .get("day_custody_day")
            .and_then(Value::as_str)
            .unwrap_or("20260101");
        return Some(DayCustodyFixture::new(day, items.clone()));
    }
    value
        .get("day_custody_absent")
        .and_then(Value::as_bool)
        .filter(|absent| *absent)
        .map(|_| DayCustodyFixture::absent("20260101"))
}

pub(crate) struct OpportunisticDefaultListenerTrap(Option<StdTcpListener>);

impl OpportunisticDefaultListenerTrap {
    pub(crate) fn bind() -> Self {
        match StdTcpListener::bind("127.0.0.1:5015") {
            Ok(listener) => {
                listener.set_nonblocking(true).unwrap();
                Self(Some(listener))
            }
            Err(error) => {
                eprintln!(
                    "criterion 12 note: opportunistic default-listener trap did not execute: {error}"
                );
                Self(None)
            }
        }
    }

    pub(crate) fn assert_zero_connections(&self) {
        if let Some(listener) = &self.0 {
            assert!(matches!(
                listener.accept(),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock
            ));
        }
    }
}

pub(crate) struct MutableClock {
    wall: AtomicU64,
    mono: AtomicU64,
}

impl MutableClock {
    pub(crate) fn new(wall: f64, mono: f64) -> Self {
        Self {
            wall: AtomicU64::new(wall.to_bits()),
            mono: AtomicU64::new(mono.to_bits()),
        }
    }

    pub(crate) fn set_wall(&self, value: f64) {
        self.wall.store(value.to_bits(), Ordering::Release);
    }

    pub(crate) fn set_mono(&self, value: f64) {
        self.mono.store(value.to_bits(), Ordering::Release);
    }
}

impl Clock for MutableClock {
    fn wall_seconds(&self) -> f64 {
        f64::from_bits(self.wall.load(Ordering::Acquire))
    }

    fn monotonic_seconds(&self) -> f64 {
        f64::from_bits(self.mono.load(Ordering::Acquire))
    }
}

struct ReceiverBody(mpsc::Receiver<Result<Bytes, std::io::Error>>);

impl Body for ReceiverBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.0.poll_recv(cx) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(Box::new(error)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Received {
    pub(crate) method: String,
    pub(crate) uri: String,
    pub(crate) headers: hyper::HeaderMap,
    pub(crate) body: Vec<u8>,
}

pub(crate) struct MockServer {
    pub(crate) url: String,
    received: Arc<Mutex<Vec<Received>>>,
    task: JoinHandle<()>,
    linked: LinkedMockServer,
}

static LINKED_FIXTURES: OnceLock<Mutex<HashMap<String, PrivateLinkCapability>>> = OnceLock::new();

pub(crate) fn linked_fixture_capability(origin: &str) -> Option<PrivateLinkCapability> {
    LINKED_FIXTURES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .get(origin)
        .cloned()
}

pub(crate) struct LinkedMockServer {
    peer: PrivateLinkPeer,
    session: PrivateLinkSession,
}

impl LinkedMockServer {
    pub(crate) async fn new(responses: Vec<(u16, Value)>) -> Self {
        Self::new_raw(
            responses
                .into_iter()
                .map(|(status, body)| (status, body.to_string().into_bytes()))
                .collect(),
        )
        .await
    }

    pub(crate) async fn new_with_facts(
        facts: crate::private_link::LinkFacts,
        responses: Vec<(u16, Value)>,
    ) -> Self {
        let peer = PrivateLinkPeer::start().await;
        for (status, body) in responses {
            peer.enqueue_response(status, body.to_string().into_bytes());
        }
        let session = crate::private_link::start_private_link_session_with_facts(
            &tempfile::tempdir().unwrap().keep(),
            peer.credential(),
            "desktop",
            facts,
        )
        .await
        .unwrap();
        Self { peer, session }
    }

    pub(crate) async fn new_raw(responses: Vec<(u16, Vec<u8>)>) -> Self {
        let peer = PrivateLinkPeer::start().await;
        for (status, body) in responses {
            peer.enqueue_response(status, body);
        }
        let session = start_private_link_session(
            &tempfile::tempdir().unwrap().keep(),
            peer.credential(),
            "desktop",
        )
        .await
        .unwrap();
        Self { peer, session }
    }

    pub(crate) fn capability(&self) -> PrivateLinkCapability {
        self.session.capability()
    }

    pub(crate) fn credential(&self) -> spl_transport::credential::Credential {
        self.peer.credential()
    }

    pub(crate) fn enqueue_day_custody(&self, fixture: DayCustodyFixture) {
        self.peer.enqueue_day_custody(fixture);
    }

    pub(crate) fn enqueue_response(&self, status: u16, body: impl Into<Vec<u8>>) {
        self.peer.enqueue_response(status, body);
    }

    pub(crate) fn enqueue_manifest_probe(&self, status: u16, body: impl Into<Vec<u8>>) {
        self.peer.enqueue_manifest_probe(status, body);
    }

    pub(crate) fn requests(&self) -> Vec<Received> {
        self.peer
            .requests()
            .into_iter()
            .map(|request| {
                let mut headers = hyper::HeaderMap::new();
                for (name, value) in request.headers {
                    if let (Ok(name), Ok(value)) = (
                        hyper::header::HeaderName::from_bytes(name.as_bytes()),
                        hyper::header::HeaderValue::from_str(&value),
                    ) {
                        headers.append(name, value);
                    }
                }
                Received {
                    method: request.method,
                    uri: request.path,
                    headers,
                    body: request.body,
                }
            })
            .collect()
    }

    pub(crate) fn request_count(&self, uri_substring: &str) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.uri.contains(uri_substring))
            .count()
    }

    pub(crate) async fn wait_for_requests(&self, count: usize) {
        self.peer.wait_for_requests(count).await;
    }

    pub(crate) fn gate_responses(&self, count: usize, gate: Arc<Notify>) {
        for _ in 0..count {
            self.peer
                .enqueue_gated_response(200, br#"{"days":{}}"#.to_vec(), gate.clone());
        }
    }
}

pub(crate) enum Action {
    Response(u16, Value),
    Raw(u16, &'static str),
    Stream(u16, mpsc::Receiver<Result<Bytes, std::io::Error>>),
}

impl MockServer {
    pub(crate) async fn new(responses: Vec<(u16, Value)>) -> Self {
        Self::new_actions(
            responses
                .into_iter()
                .map(|(status, body)| Action::Response(status, body))
                .collect(),
        )
        .await
    }

    pub(crate) async fn new_actions(responses: Vec<Action>) -> Self {
        let linked = LinkedMockServer::new_raw(Vec::new()).await;
        for action in &responses {
            match action {
                Action::Response(status, body) if *status == 200 => {
                    if let Some(fixture) = day_custody_fixture(body) {
                        linked.enqueue_day_custody(fixture);
                    } else {
                        linked.enqueue_response(*status, body.to_string());
                    }
                }
                Action::Response(status, body) => {
                    linked.enqueue_response(*status, body.to_string())
                }
                Action::Raw(status, body) => linked.enqueue_response(*status, *body),
                Action::Stream(status, _) => linked.enqueue_response(*status, Vec::new()),
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        LINKED_FIXTURES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .insert(url.clone(), linked.capability());
        let received = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        let task_received = Arc::clone(&received);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let received = Arc::clone(&task_received);
                let queue = Arc::clone(&queue);
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let received = Arc::clone(&received);
                        let queue = Arc::clone(&queue);
                        async move {
                            let (parts, body) = request.into_parts();
                            let body = body.collect().await.unwrap().to_bytes().to_vec();
                            received.lock().unwrap().push(Received {
                                method: parts.method.to_string(),
                                uri: parts.uri.to_string(),
                                headers: parts.headers,
                                body,
                            });
                            let action = queue
                                .lock()
                                .unwrap()
                                .pop_front()
                                .unwrap_or(Action::Response(500, Value::Null));
                            let (status, body): (
                                u16,
                                http_body_util::combinators::BoxBody<Bytes, BoxError>,
                            ) = match action {
                                Action::Response(status, body) => (
                                    status,
                                    Full::new(Bytes::from(body.to_string()))
                                        .map_err(|never| match never {})
                                        .boxed(),
                                ),
                                Action::Raw(status, body) => (
                                    status,
                                    Full::new(Bytes::from(body))
                                        .map_err(|never| match never {})
                                        .boxed(),
                                ),
                                Action::Stream(status, receiver) => {
                                    (status, ReceiverBody(receiver).boxed())
                                }
                            };
                            Ok::<_, std::io::Error>(
                                Response::builder()
                                    .status(status)
                                    .header("content-type", "application/json")
                                    .body(body)
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self {
            url,
            received,
            task,
            linked,
        }
    }

    pub(crate) fn requests(&self) -> Vec<Received> {
        let linked = self.linked.requests();
        if linked.is_empty() {
            self.received.lock().unwrap().clone()
        } else {
            linked
        }
    }

    pub(crate) fn enqueue_day_custody(&self, fixture: DayCustodyFixture) {
        self.linked.enqueue_day_custody(fixture);
    }

    pub(crate) fn enqueue_manifest_probe(&self, status: u16, body: impl Into<Vec<u8>>) {
        self.linked.enqueue_manifest_probe(status, body);
    }

    pub(crate) fn request_count(&self, uri_substring: &str) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.uri.contains(uri_substring))
            .count()
    }

    pub(crate) async fn gated() -> (Self, Arc<Notify>) {
        let linked = LinkedMockServer::new_raw(Vec::new()).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        LINKED_FIXTURES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .insert(url.clone(), linked.capability());
        let received = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Notify::new());
        let task_received = Arc::clone(&received);
        let task_gate = Arc::clone(&gate);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let received = Arc::clone(&task_received);
                let gate = Arc::clone(&task_gate);
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let received = Arc::clone(&received);
                        let gate = Arc::clone(&gate);
                        async move {
                            let (parts, body) = request.into_parts();
                            let body = body.collect().await.unwrap().to_bytes().to_vec();
                            received.lock().unwrap().push(Received {
                                method: parts.method.to_string(),
                                uri: parts.uri.to_string(),
                                headers: parts.headers,
                                body,
                            });
                            gate.notified().await;
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("{}"))))
                        }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        linked.gate_responses(32, gate.clone());
        (
            Self {
                url,
                received,
                task,
                linked,
            },
            gate,
        )
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        if let Some(fixtures) = LINKED_FIXTURES.get() {
            fixtures.lock().unwrap().remove(&self.url);
        }
        self.task.abort();
    }
}

pub(crate) async fn wait_for_requests(server: &MockServer, count: usize) {
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while server.requests().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "timed out waiting for {count} requests; saw {}",
        server.requests().len()
    );
}

/// Waits for a real loopback progress point under a host-clock bound.
///
/// `PROGRESS_BOUND` is a liveness bound, not a latency budget: local
/// TCP/TLS/HTTP work normally settles in milliseconds, so a healthy run never
/// consumes it. It is deliberately not tied to `LAN_CARRIER_TIMEOUT`.
/// `tokio::time::timeout` polls its inner future before checking its deadline,
/// so a completed LAN dial still wins even if the product budget has nominally
/// elapsed. Here the wait runs in real milliseconds rather than tens of virtual
/// seconds; if LAN acceptance ever loses that race, the test fails with a
/// bounded diagnostic instead of hanging.
///
/// Call only from `#[tokio::test(start_paused = true)]` and only for real-I/O
/// waits: this helper resumes Tokio time, applies a real `timeout`, then restores
/// paused time. Tokio `pause` and `resume` panic in another time mode. A timeout
/// while time remains paused is not a real bound because Tokio may auto-advance
/// its virtual clock while idle. The passed work must not call `pause` or
/// `resume` itself.
pub(crate) async fn bounded_progress<F>(
    bound: Duration,
    progress_point: &str,
    work: F,
) -> Result<F::Output, String>
where
    F: Future,
{
    tokio::time::resume();
    let restore = RestorePausedClock;
    let result = tokio::time::timeout(bound, work).await;
    drop(restore);
    result.map_err(|_| format!("{progress_point} not reached within {bound:?} of real time"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // AC: shared HTTP harness reports URI-specific hit counts for phase gates.
    #[tokio::test]
    async fn counts_requests_by_uri_substring() {
        let server = MockServer::new(vec![(200, json!({}))]).await;
        reqwest::get(format!("{}/app/devices/segments?day=20260101", server.url))
            .await
            .unwrap();
        wait_for_requests(&server, 1).await;
        assert_eq!(server.request_count("/app/devices/segments"), 1);
        assert_eq!(server.request_count("/app/devices/ingest"), 0);
    }

    // AC: shared HTTP harness can emit test-controlled streaming chunks.
    #[tokio::test]
    async fn streams_receiver_chunks() {
        let (sender, receiver) = mpsc::channel(2);
        let server = MockServer::new_actions(vec![Action::Stream(200, receiver)]).await;
        let request = tokio::spawn(reqwest::get(server.url.clone()));
        sender.send(Ok(Bytes::from_static(b"one"))).await.unwrap();
        sender.send(Ok(Bytes::from_static(b"two"))).await.unwrap();
        drop(sender);
        let body = request.await.unwrap().unwrap().bytes().await.unwrap();
        assert_eq!(body, Bytes::from_static(b"onetwo"));
    }

    // AC: real-I/O progress waits restore deterministic paused time after their signal arrives.
    #[tokio::test(start_paused = true)]
    async fn bounded_progress_waits_for_test_owned_signal() {
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let changed = Arc::new(Notify::new());
        let worker = tokio::spawn({
            let ready = ready.clone();
            let changed = changed.clone();
            async move {
                tokio::task::yield_now().await;
                ready.store(true, Ordering::Release);
                changed.notify_one();
            }
        });
        bounded_progress(PROGRESS_BOUND, "test-owned progress", async {
            loop {
                let notified = changed.notified();
                if ready.load(Ordering::Acquire) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
        worker.await.unwrap();
        let start = tokio::time::Instant::now();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(start.elapsed(), Duration::from_secs(1));
    }

    // AC: unreachable real-I/O progress reports its named host-clock bound.
    #[tokio::test(start_paused = true)]
    async fn bounded_progress_reports_unreachable_progress() {
        let error = bounded_progress(
            Duration::from_millis(250),
            "unreachable test progress",
            std::future::pending::<()>(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error,
            "unreachable test progress not reached within 250ms of real time"
        );
    }
}
