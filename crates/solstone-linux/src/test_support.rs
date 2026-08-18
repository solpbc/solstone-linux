// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    observer::Clock,
    private_link::{
        ObserverState, PrivateLinkCapability, PrivateLinkSession, publish_observer_registration,
        start_private_link_session,
    },
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
    io,
    net::TcpListener as StdTcpListener,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc},
    task::JoinHandle,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

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
        publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: peer.credential().instance_id,
                key: "K".to_owned(),
                prefix: "prefix".to_owned(),
                name: "desktop".to_owned(),
                ingest_url: "/app/devices/ingest".to_owned(),
                protocol_version: 2,
            },
        )
        .unwrap();
        Self { peer, session }
    }

    pub(crate) fn capability(&self) -> PrivateLinkCapability {
        self.session.capability("/app/devices/ingest".to_owned())
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
                .enqueue_gated_response(200, b"{}".to_vec(), gate.clone());
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
        let linked_responses = responses
            .iter()
            .map(|action| match action {
                Action::Response(status, body) => (*status, body.to_string().into_bytes()),
                Action::Raw(status, body) => (*status, body.as_bytes().to_vec()),
                Action::Stream(status, _) => (*status, Vec::new()),
            })
            .collect();
        let linked = LinkedMockServer::new_raw(linked_responses).await;
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
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while server.requests().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
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
}
