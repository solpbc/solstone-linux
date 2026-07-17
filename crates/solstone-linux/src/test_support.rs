// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

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
    collections::VecDeque,
    convert::Infallible,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc},
    task::JoinHandle,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

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
}

pub(crate) enum Action {
    Response(u16, Value),
    Raw(u16, &'static str),
    Disconnect,
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
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
                                Action::Disconnect => {
                                    return Err::<
                                        Response<
                                            http_body_util::combinators::BoxBody<Bytes, BoxError>,
                                        >,
                                        _,
                                    >(
                                        std::io::Error::new(
                                            std::io::ErrorKind::ConnectionAborted,
                                            "mock disconnect",
                                        ),
                                    );
                                }
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
        }
    }

    pub(crate) fn requests(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }

    pub(crate) fn request_count(&self, uri_substring: &str) -> usize {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.uri.contains(uri_substring))
            .count()
    }

    pub(crate) async fn gated() -> (Self, Arc<Notify>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
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
        (
            Self {
                url,
                received,
                task,
            },
            gate,
        )
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
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
        reqwest::get(format!("{}/app/observer/segments?day=20260101", server.url))
            .await
            .unwrap();
        wait_for_requests(&server, 1).await;
        assert_eq!(server.request_count("/app/observer/segments"), 1);
        assert_eq!(server.request_count("/app/observer/ingest"), 0);
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
