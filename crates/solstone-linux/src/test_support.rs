// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response,
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

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
                            let (status, body) = match action {
                                Action::Response(status, body) => (status, body.to_string()),
                                Action::Raw(status, body) => (status, body.to_owned()),
                                Action::Disconnect => {
                                    return Err::<Response<Full<Bytes>>, _>(std::io::Error::new(
                                        std::io::ErrorKind::ConnectionAborted,
                                        "mock disconnect",
                                    ));
                                }
                            };
                            Ok::<_, std::io::Error>(
                                Response::builder()
                                    .status(status)
                                    .header("content-type", "application/json")
                                    .body(Full::new(Bytes::from(body)))
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
}
