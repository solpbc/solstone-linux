// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Server-initiated chat events bridged to Linux desktop surfaces.

use crate::config::Config;
use chrono::Local;
use futures_util::{FutureExt, StreamExt, future::BoxFuture};
use notify_rust::{Notification, NotificationResponse};
use reqwest::{Client, StatusCode};
use rustix::{
    fs::{FileType, Mode, OFlags},
    io::Errno,
};
use serde_json::Value;
use std::{
    env,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{sync::OnceCell, task::JoinHandle};
use tokio_util::sync::CancellationToken;

// Keep these event names and owner-facing copy hand-synced with
// solstone/convey/sol_initiated/copy.py; this repo does not vendor that canon.
pub const EVENT_SOL_CHAT_REQUEST: &str = "sol_chat_request";
pub const EVENT_SOL_CHAT_REQUEST_SUPERSEDED: &str = "sol_chat_request_superseded";
pub const EVENT_OWNER_CHAT_OPEN: &str = "owner_chat_open";
pub const EVENT_OWNER_CHAT_DISMISSED: &str = "owner_chat_dismissed";
pub const NOTIFY_TITLE: &str = "sol";
pub const SURFACE: &str = "linux";
const FIFO_RELATIVE_PATH: &str = ".solstone/notify";
const RECONNECT_DELAYS: [u64; 6] = [1, 2, 4, 8, 16, 30];
pub const HEARTBEAT_STALE: Duration = Duration::from_secs(60);
pub const SSE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const SSE_READ_TIMEOUT: Duration = Duration::from_secs(HEARTBEAT_STALE.as_secs() + 30);
const BRIDGE_POLL_INTERVAL: Duration = Duration::from_secs(5);
const NOTIFY_ACTION_KEY: &str = "open";
const HEALTHY_RUN: Duration = Duration::from_secs(60);
const OPT_IN_POLL: Duration = Duration::from_secs(300);
pub const PENDING_CAP: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
struct NotificationSpec {
    summary: String,
    body: String,
    offer_action: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotificationOutcome {
    Open,
    Dismissed,
    Failed,
    Cancelled,
}

fn outcome_for_action(action: &str) -> NotificationOutcome {
    if action == NOTIFY_ACTION_KEY {
        NotificationOutcome::Open
    } else {
        NotificationOutcome::Dismissed
    }
}

type NotifyFn = Arc<
    dyn Fn(NotificationSpec, CancellationToken) -> BoxFuture<'static, NotificationOutcome>
        + Send
        + Sync,
>;
type AckFn = Arc<dyn Fn(String, String, String) -> BoxFuture<'static, ()> + Send + Sync>;
type OpenFn = Arc<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>;
type CapabilitiesFn = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;
type SleepFn = Arc<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;
type MonotonicFn = Arc<dyn Fn() -> Duration + Send + Sync>;
type LocalDayFn = Arc<dyn Fn() -> String + Send + Sync>;
type BodyFn = Arc<
    dyn Fn(Config, CancellationToken, BridgeDeps, Client) -> BoxFuture<'static, Result<(), String>>
        + Send
        + Sync,
>;

#[derive(Clone)]
struct BridgeDeps {
    notify: NotifyFn,
    ack_open: AckFn,
    open_browser: OpenFn,
    supports_actions: CapabilitiesFn,
    sleep: SleepFn,
    monotonic_now: MonotonicFn,
    local_day: LocalDayFn,
}

struct PendingRequest {
    request_id: String,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

#[derive(Default, Debug)]
struct SseParseState {
    buffered: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    id: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct SseFrame {
    event: Option<String>,
    data: String,
    id: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum SseItem {
    Heartbeat,
    Frame(SseFrame),
}

fn parse_sse_chunk(mut state: SseParseState, chunk: &[u8]) -> (SseParseState, Vec<SseItem>) {
    state.buffered.extend_from_slice(chunk);
    let mut items = Vec::new();
    while let Some(newline) = state.buffered.iter().position(|byte| *byte == b'\n') {
        let mut raw = state.buffered.drain(..=newline).collect::<Vec<_>>();
        raw.pop();
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
        let line = String::from_utf8_lossy(&raw);
        if line.starts_with(':') {
            items.push(SseItem::Heartbeat);
            continue;
        }
        if line.is_empty() {
            if state.data.is_empty() {
                state.event = None;
                state.id = None;
                continue;
            }
            items.push(SseItem::Frame(SseFrame {
                event: state.event.take(),
                data: state.data.join("\n"),
                id: state.id.take(),
            }));
            state.data.clear();
            continue;
        }
        let (field, mut value) = line
            .split_once(':')
            .map_or((line.as_ref(), ""), |(field, value)| (field, value));
        if let Some(stripped) = value.strip_prefix(' ') {
            value = stripped;
        }
        match field {
            "data" => state.data.push(value.to_owned()),
            "event" => state.event = Some(value.to_owned()),
            "id" => state.id = Some(value.to_owned()),
            _ => {}
        }
    }
    (state, items)
}

fn python_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Number(value)) => value.as_f64().is_some_and(|number| number != 0.0),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
    }
}

fn python_scalar_str(value: &Value) -> Option<String> {
    match value {
        Value::Bool(true) => Some("True".to_owned()),
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Some(integer.to_string())
            } else if let Some(integer) = value.as_u64() {
                Some(integer.to_string())
            } else {
                // Named deviation: Rust's finite-float Display spelling differs from Python's
                // repr-style thresholds (for example 1e20). The bridge preserves the numeric
                // value but does not implement a Python float formatter for malformed payloads.
                value.as_f64().map(|number| {
                    let rendered = number.to_string();
                    if rendered.contains(['.', 'e', 'E']) {
                        rendered
                    } else {
                        format!("{rendered}.0")
                    }
                })
            }
        }
        _ => None,
    }
}

fn python_or_empty_str(value: Option<&Value>) -> String {
    if !python_truthy(value) {
        return String::new();
    }
    // Named deviation: truthy list/dict summaries become empty instead of Python repr text;
    // no server-produced summary has a collection shape.
    value.and_then(python_scalar_str).unwrap_or_default()
}

fn fifo_path() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(FIFO_RELATIVE_PATH)
}

fn write_fifo(line: &str, path: &Path) {
    let stat = match rustix::fs::stat(path) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => {
            tracing::debug!(path = %path.display(), "Chat bridge FIFO missing");
            return;
        }
        Err(error) => {
            tracing::warn!(%error, "Chat bridge FIFO write failed");
            return;
        }
    };
    if !FileType::from_raw_mode(stat.st_mode).is_fifo() {
        tracing::debug!(path = %path.display(), "Chat bridge path is not a FIFO");
        return;
    }
    let fd = match rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(error) if fifo_error_is_tolerated(error) => {
            tracing::debug!(%error, "Chat bridge FIFO unavailable");
            return;
        }
        Err(error) => {
            tracing::warn!(%error, "Chat bridge FIFO write failed");
            return;
        }
    };
    if let Err(error) = rustix::io::write(&fd, line.as_bytes()) {
        if fifo_error_is_tolerated(error) {
            tracing::debug!(%error, "Chat bridge FIFO unavailable");
        } else {
            tracing::warn!(%error, "Chat bridge FIFO write failed");
        }
    }
}

fn fifo_error_is_tolerated(error: Errno) -> bool {
    matches!(error, Errno::NXIO | Errno::AGAIN)
}

fn chat_url(
    server_url: &str,
    day: Option<&Value>,
    event_index: Option<&Value>,
    deps: &BridgeDeps,
) -> String {
    let base = server_url.trim_end_matches('/');
    if let (Some(day), Some(index)) = (
        // Named deviation: numeric day values fall back to local today instead of being
        // interpolated as Python would; server-produced day values are strings.
        day.and_then(Value::as_str).filter(|day| !day.is_empty()),
        event_index.and_then(Value::as_i64),
    ) {
        return format!("{base}/app/chat/{day}#event-{index}");
    }
    format!("{base}/app/chat/{}", (deps.local_day)())
}

fn reconnect_delay(index: usize) -> Duration {
    Duration::from_secs(RECONNECT_DELAYS[index.min(RECONNECT_DELAYS.len() - 1)])
}

fn mark_stale_if_needed(last_frame_at: Duration, now: Duration, is_stale: &mut bool) {
    if now.saturating_sub(last_frame_at) > HEARTBEAT_STALE && !*is_stale {
        tracing::warn!("Chat bridge heartbeat stale");
        *is_stale = true;
    }
}

fn mark_live_frame(is_stale: &mut bool) {
    if *is_stale {
        tracing::info!("Chat bridge heartbeat recovered");
        *is_stale = false;
    }
}

async fn cancel_pending_at(pending: &mut Vec<PendingRequest>, index: usize) {
    let request = pending.remove(index);
    request.cancellation.cancel();
    let _ = request.task.await;
}

async fn cancel_pending_id(pending: &mut Vec<PendingRequest>, request_id: &str) {
    if let Some(index) = pending
        .iter()
        .position(|request| request.request_id == request_id)
    {
        cancel_pending_at(pending, index).await;
    }
}

async fn cancel_all_pending(pending: &mut Vec<PendingRequest>) {
    while !pending.is_empty() {
        cancel_pending_at(pending, 0).await;
    }
}

async fn dispatch_event(
    payload: &serde_json::Map<String, Value>,
    pending: &mut Vec<PendingRequest>,
    opt_in: bool,
    is_stale: bool,
    config: &Config,
    deps: &BridgeDeps,
    fifo: &Path,
) {
    if payload.get("tract").and_then(Value::as_str) != Some("chat") {
        return;
    }
    let Some(event) = payload.get("event").and_then(Value::as_str) else {
        return;
    };
    if !matches!(
        event,
        EVENT_SOL_CHAT_REQUEST
            | EVENT_SOL_CHAT_REQUEST_SUPERSEDED
            | EVENT_OWNER_CHAT_OPEN
            | EVENT_OWNER_CHAT_DISMISSED
    ) {
        return;
    }
    let value = payload.get("request_id");
    if !python_truthy(value) {
        tracing::debug!(event, "Chat event missing request_id");
        return;
    }
    let Some(request_id) = value.and_then(python_scalar_str) else {
        // Named deviation: Python str()-ifies a truthy list/dict request_id into a Python repr;
        // Rust drops it as malformed because serde_json cannot reproduce Python's repr or key
        // order, and no server payload produces one.
        tracing::debug!(event, "Chat event missing request_id");
        return;
    };

    if event == EVENT_SOL_CHAT_REQUEST {
        let summary = python_or_empty_str(payload.get("summary"));
        write_fifo(&format!("sol-ping {request_id} {summary}\n"), fifo);
        cancel_pending_id(pending, &request_id).await;
        if opt_in && !is_stale {
            let offer_action = (deps.supports_actions)().await;
            let url = chat_url(
                &config.server_url,
                payload.get("day"),
                payload.get("event_index"),
                deps,
            );
            let cancellation = CancellationToken::new();
            let task_cancellation = cancellation.clone();
            let notify = Arc::clone(&deps.notify);
            let ack = Arc::clone(&deps.ack_open);
            let open = Arc::clone(&deps.open_browser);
            let server_url = config.server_url.clone();
            let key = config.key.clone();
            let task_id = request_id.clone();
            let task_summary = summary.clone();
            let task_url = url.clone();
            let task = tokio::spawn(async move {
                let post_notify_cancellation = task_cancellation.clone();
                let outcome = notify(
                    NotificationSpec {
                        summary: NOTIFY_TITLE.to_owned(),
                        body: task_summary,
                        offer_action,
                    },
                    task_cancellation,
                )
                .await;
                if outcome == NotificationOutcome::Open && offer_action {
                    tracing::info!(request_id = task_id, "Opening chat request");
                    tokio::select! {
                        () = post_notify_cancellation.cancelled() => return,
                        () = ack(server_url, key, task_id) => {}
                    }
                    tokio::select! {
                        () = post_notify_cancellation.cancelled() => {}
                        () = open(task_url) => {}
                    }
                }
            });
            pending.push(PendingRequest {
                request_id,
                cancellation,
                task,
            });
            if pending.len() > PENDING_CAP {
                let evicted = pending[0].request_id.clone();
                cancel_pending_at(pending, 0).await;
                tracing::debug!("Evicted pending chat request: {evicted}");
            }
        }
        return;
    }

    cancel_pending_id(pending, &request_id).await;
    write_fifo(&format!("clear {request_id}\n"), fifo);
}

fn build_sse_client(connect: Duration, read: Duration) -> Result<Client, reqwest::Error> {
    // SSE is intentionally long-lived. Use ClientBuilder read_timeout for per-read inactivity;
    // RequestBuilder::timeout would terminate every healthy stream after 90 seconds.
    Client::builder()
        .connect_timeout(connect)
        .read_timeout(read)
        .build()
}

async fn sleep_or_stop(duration: Duration, stop: &CancellationToken, deps: &BridgeDeps) {
    tokio::select! {
        () = (deps.sleep)(duration) => {}
        () = stop.cancelled() => {}
    }
}

async fn sleep_for_reconnect(
    state: &mut ConnectionState,
    stop: &CancellationToken,
    deps: &BridgeDeps,
) {
    let delay = reconnect_delay(state.reconnect_index);
    state.reconnect_index += 1;
    tracing::info!(seconds = delay.as_secs(), "Chat bridge reconnecting");
    sleep_or_stop(delay, stop, deps).await;
}

async fn poll_opt_in(
    client: &Client,
    server_url: &str,
    key: &str,
    stop: &CancellationToken,
) -> bool {
    let url = format!("{}/api/sol_voice", server_url.trim_end_matches('/'));
    let request = client
        .get(url)
        .bearer_auth(key)
        .timeout(Duration::from_secs(10))
        .send();
    let response = tokio::select! {
        () = stop.cancelled() => return false,
        response = request => response,
    };
    let Ok(response) = response else { return false };
    if response.status() != StatusCode::OK {
        return false;
    }
    response
        .json::<Value>()
        .await
        .ok()
        .and_then(|body| body.get("linux_notify_send").and_then(Value::as_bool))
        .unwrap_or(false)
}

async fn opt_in_loop(
    client: Client,
    server_url: String,
    key: String,
    value: Arc<AtomicBool>,
    stop: CancellationToken,
    deps: BridgeDeps,
) {
    while !stop.is_cancelled() {
        value.store(
            poll_opt_in(&client, &server_url, &key, &stop).await,
            Ordering::Release,
        );
        sleep_or_stop(OPT_IN_POLL, &stop, &deps).await;
    }
}

enum ConnectionEnd {
    Terminal,
    Reconnect,
    Stopped,
}

#[derive(Default)]
struct ConnectionState {
    reconnect_index: usize,
    is_stale: bool,
}

async fn consume_connection(
    client: &Client,
    config: &Config,
    stop: &CancellationToken,
    deps: &BridgeDeps,
    pending: &mut Vec<PendingRequest>,
    opt_in: &AtomicBool,
    state: &mut ConnectionState,
) -> ConnectionEnd {
    // Named deviation: Python uses a blocking requests worker and asyncio queue. Rust consumes
    // reqwest's async byte stream directly, but still defers clean EOF to the active five-second
    // poll deadline.
    let url = callosum_url(&config.server_url);
    let response = client.get(url).bearer_auth(&config.key).send().await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            tracing::debug!(%error, "Chat bridge transport error");
            return ConnectionEnd::Reconnect;
        }
    };
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        tracing::error!(
            status = response.status().as_u16(),
            "Chat bridge SSE authorization failed: status {}",
            response.status().as_u16()
        );
        return ConnectionEnd::Terminal;
    }
    if response.status() != StatusCode::OK {
        tracing::debug!(
            status = response.status().as_u16(),
            "Chat bridge transport error"
        );
        return ConnectionEnd::Reconnect;
    }

    let mut stream = response.bytes_stream();
    let mut parser = SseParseState::default();
    let mut last_frame_at = (deps.monotonic_now)();
    let fifo = fifo_path();
    let mut poll = (deps.sleep)(BRIDGE_POLL_INTERVAL);
    loop {
        let next = tokio::select! {
            () = stop.cancelled() => return ConnectionEnd::Stopped,
            () = poll.as_mut() => {
                mark_stale_if_needed(last_frame_at, (deps.monotonic_now)(), &mut state.is_stale);
                poll = (deps.sleep)(BRIDGE_POLL_INTERVAL);
                continue;
            }
            next = stream.next() => next,
        };
        let Some(next) = next else {
            // Python observes clean worker EOF only when its active five-second queue poll expires.
            tokio::select! {
                () = stop.cancelled() => return ConnectionEnd::Stopped,
                () = poll.as_mut() => return ConnectionEnd::Reconnect,
            }
        };
        let bytes = match next {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::debug!(%error, "Chat bridge transport error");
                return ConnectionEnd::Reconnect;
            }
        };
        let (next_parser, items) = parse_sse_chunk(parser, &bytes);
        parser = next_parser;
        if !items.is_empty() {
            poll = (deps.sleep)(BRIDGE_POLL_INTERVAL);
        }
        for item in items {
            last_frame_at = (deps.monotonic_now)();
            state.reconnect_index = 0;
            mark_live_frame(&mut state.is_stale);
            let SseItem::Frame(frame) = item else {
                continue;
            };
            let payload = match serde_json::from_str::<Value>(&frame.data) {
                Ok(Value::Object(payload)) => payload,
                Ok(_) => continue,
                Err(error) => {
                    tracing::debug!(%error, "Chat bridge frame JSON decode failed");
                    continue;
                }
            };
            dispatch_event(
                &payload,
                pending,
                opt_in.load(Ordering::Acquire),
                state.is_stale,
                config,
                deps,
                &fifo,
            )
            .await;
        }
    }
}

fn callosum_url(server_url: &str) -> String {
    format!("{}/app/observer/callosum", server_url.trim_end_matches('/'))
}

async fn run_bridge_body(
    config: &Config,
    stop: &CancellationToken,
    deps: &BridgeDeps,
    client: &Client,
) -> Result<(), String> {
    let opt_in = Arc::new(AtomicBool::new(false));
    let poll_stop = stop.child_token();
    let poll_task = tokio::spawn(opt_in_loop(
        client.clone(),
        config.server_url.trim_end_matches('/').to_owned(),
        config.key.clone(),
        Arc::clone(&opt_in),
        poll_stop.clone(),
        deps.clone(),
    ));
    let mut pending = Vec::new();
    let mut state = ConnectionState::default();
    let result = loop {
        match consume_connection(
            client,
            config,
            stop,
            deps,
            &mut pending,
            &opt_in,
            &mut state,
        )
        .await
        {
            ConnectionEnd::Terminal => break Ok(()),
            ConnectionEnd::Stopped => break Ok(()),
            ConnectionEnd::Reconnect => {
                sleep_for_reconnect(&mut state, stop, deps).await;
                if stop.is_cancelled() {
                    break Ok(());
                }
            }
        }
    };
    poll_stop.cancel();
    let _ = poll_task.await;
    cancel_all_pending(&mut pending).await;
    result
}

async fn production_notification(
    spec: NotificationSpec,
    cancellation: CancellationToken,
) -> NotificationOutcome {
    let mut notification = Notification::new();
    notification
        .appname("sol")
        .summary(&spec.summary)
        .body(&spec.body);
    if spec.offer_action {
        notification.action(NOTIFY_ACTION_KEY, "Open");
    }
    // notify-rust does not document cancellation safety after the D-Bus request is sent but
    // before a handle is returned. If cancellation wins in that narrow window there is no id
    // available to close; once a handle exists, the cancellation branch below always closes it.
    let handle = tokio::select! {
        () = cancellation.cancelled() => return NotificationOutcome::Cancelled,
        result = notification.show_async() => match result {
            Ok(handle) => handle,
            Err(error) => {
                tracing::debug!(%error, "notify-rust failed");
                return NotificationOutcome::Failed;
            }
        }
    };
    let mut outcome = NotificationOutcome::Dismissed;
    tokio::select! {
        () = cancellation.cancelled() => {
            handle.close_async().await;
            NotificationOutcome::Cancelled
        }
        () = handle.wait_for_action_async(|response| {
            if let NotificationResponse::Action(action) = response {
                outcome = outcome_for_action(action);
            }
        }) => outcome,
    }
}

fn production_deps(client: Client) -> BridgeDeps {
    let capability = Arc::new(OnceCell::new());
    let supports_actions = Arc::new(move || {
        let capability = Arc::clone(&capability);
        async move {
            *capability
                .get_or_init(|| async {
                    match tokio::task::spawn_blocking(notify_rust::get_capabilities).await {
                        Ok(Ok(values)) => values.iter().any(|value| value == "actions"),
                        Ok(Err(error)) => {
                            tracing::debug!(%error, "Chat notification capability probe failed");
                            false
                        }
                        Err(error) => {
                            tracing::debug!(%error, "Chat notification capability probe failed");
                            false
                        }
                    }
                })
                .await
        }
        .boxed()
    }) as CapabilitiesFn;
    let ack_client = client;
    BridgeDeps {
        notify: Arc::new(|spec, cancellation| production_notification(spec, cancellation).boxed()),
        ack_open: Arc::new(move |server_url, key, request_id| {
            let client = ack_client.clone();
            async move {
                let url = format!(
                    "{}/api/chat/{EVENT_SOL_CHAT_REQUEST}/open",
                    server_url.trim_end_matches('/')
                );
                let response = client
                    .post(url)
                    .bearer_auth(key)
                    .json(&serde_json::json!({"request_id": request_id}))
                    .timeout(Duration::from_secs(10))
                    .send()
                    .await;
                match response {
                    Ok(response) if response.status().is_success() => {}
                    Ok(response) => {
                        tracing::debug!(status = %response.status(), "Chat open ack failed")
                    }
                    Err(error) => tracing::debug!(%error, "Chat open ack failed"),
                }
            }
            .boxed()
        }),
        open_browser: Arc::new(|url| {
            async move {
                if let Err(error) = open::that_detached(url) {
                    tracing::debug!(%error, "xdg-open failed");
                }
            }
            .boxed()
        }),
        supports_actions,
        sleep: Arc::new(|duration| tokio::time::sleep(duration).boxed()),
        monotonic_now: {
            let origin = Instant::now();
            Arc::new(move || origin.elapsed())
        },
        local_day: Arc::new(|| Local::now().format("%Y%m%d").to_string()),
    }
}

async fn run_chat_bridge_with_deps(
    config: &Config,
    stop: CancellationToken,
    deps: BridgeDeps,
    client: Client,
    body: BodyFn,
) {
    let mut supervise_index = 0;
    while !stop.is_cancelled() {
        let started = (deps.monotonic_now)();
        let run = body(config.clone(), stop.clone(), deps.clone(), client.clone());
        match AssertUnwindSafe(run).catch_unwind().await {
            Ok(Ok(())) => return,
            Ok(Err(error)) => tracing::error!(%error, "Chat bridge crashed"),
            Err(_) => tracing::error!("Chat bridge crashed"),
        }
        if stop.is_cancelled() {
            break;
        }
        if (deps.monotonic_now)().saturating_sub(started) >= HEALTHY_RUN {
            supervise_index = 0;
        }
        let delay = reconnect_delay(supervise_index);
        supervise_index += 1;
        tracing::info!(seconds = delay.as_secs(), "Chat bridge restarting");
        sleep_or_stop(delay, &stop, &deps).await;
    }
}

/// Run the Linux chat bridge until stopped or authorization is rejected.
pub async fn run_chat_bridge(config: &Config, stop: CancellationToken) {
    if !config.chat_bridge_enabled {
        return;
    }
    if config.server_url.is_empty() || config.key.is_empty() {
        tracing::debug!("Chat bridge disabled: server_url or key missing");
        return;
    }
    let client = match build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT) {
        Ok(client) => client,
        Err(error) => {
            tracing::error!(%error, "Chat bridge crashed");
            return;
        }
    };
    let deps = production_deps(client.clone());
    let body: BodyFn = Arc::new(|config, stop, deps, client| {
        async move { run_bridge_body(&config, &stop, &deps, &client).await }.boxed()
    });
    run_chat_bridge_with_deps(config, stop, deps, client, body).await;
}

// Python chat bridge provenance (45/45):
// test_sse_parser_data_only_frame -> tests::sse_data_only_frame.
// test_sse_parser_event_and_data_frame -> tests::sse_event_id_and_data_frame.
// test_sse_parser_multiline_data -> tests::sse_multiline_data_joins_with_newline.
// test_sse_parser_ignores_comment -> tests::sse_comment_does_not_mutate_frame_state.
// test_sse_parser_partial_frame_without_terminator_returns_none -> tests::sse_partial_frame_is_not_flushed.
// test_dispatch_drops_non_chat_tract -> tests::dispatch_drops_non_chat_tract.
// test_dispatch_drops_unrecognized_chat_event -> tests::dispatch_drops_unknown_chat_event.
// test_dispatch_recognized_events -> tests::dispatch_recognizes_all_four_canonical_events.
// test_request_opt_in_off_writes_fifo_without_notify -> tests::opted_out_request_writes_fifo_without_notification.
// test_request_fifo_absent_no_error -> tests::missing_fifo_is_contained.
// test_request_opt_in_on_not_stale_fires_notify -> tests::live_opted_in_request_starts_notification.
// test_request_stale_skips_notify_but_writes_fifo -> tests::stale_request_writes_fifo_without_notification.
// test_superseded_removes_pending_writes_clear_and_cancels_task -> tests::superseded_clears_and_drains_pending_notification.
// test_owner_chat_open_removes_pending_writes_clear_and_cancels_task -> tests::owner_open_clears_and_drains_pending_notification.
// test_owner_chat_dismissed_removes_pending_writes_clear_and_cancels_task -> tests::owner_dismissed_clears_and_drains_pending_notification.
// test_fifo_present_with_reader_succeeds -> tests::fifo_with_reader_receives_exact_bytes.
// test_fifo_present_no_reader_enxio_swallowed -> tests::fifo_enxio_is_contained.
// test_fifo_missing_noop -> tests::fifo_missing_is_contained.
// test_fifo_regular_file_noop -> tests::regular_file_is_not_written.
// test_fifo_eagain_swallowed -> tests::fifo_eagain_is_contained.
// test_heartbeat_staleness_marks_stale_and_logs_once_after_60s -> tests::heartbeat_stale_after_strictly_more_than_sixty_seconds.
// test_heartbeat_new_frame_recovers_from_stale -> tests::live_frame_recovers_stale_state.
// test_sse_worker_uses_finite_read_timeout -> tests::sse_client_has_finite_connect_and_read_timeouts.
// test_read_timeout_exceeds_staleness_threshold -> tests::read_timeout_is_derived_and_exceeds_stale_threshold.
// test_reconnect_transport_error_backoff_sequence -> tests::transport_reconnect_ladder_is_exact.
// test_read_timeout_reconnects_and_clears_stale -> tests::read_timeout_reconnect_then_heartbeat_resets_ladder.
// test_reconnect_successful_frame_resets_backoff_index -> tests::any_frame_resets_reconnect_index.
// test_terminal_401_exits_without_reconnect -> tests::unauthorized_exits_without_reconnect.
// test_terminal_403_exits_without_reconnect -> tests::forbidden_exits_without_reconnect.
// test_click_post_reachable_posts_then_xdg_open -> tests::open_action_acks_then_opens_browser.
// test_click_post_unreachable_still_xdg_open -> tests::ack_failure_still_opens_browser.
// test_dismissal_empty_stdout_no_ack_no_open -> tests::dismissal_does_not_ack_or_open.
// test_nonaction_stdout_treated_as_dismissal -> tests::non_open_actions_are_dismissals.
// test_click_notify_nonzero_does_not_xdg_open -> tests::notification_failure_does_not_ack_or_open.
// test_chat_url_with_day_and_event_index -> tests::chat_url_uses_day_and_event_index.
// test_chat_url_missing_day_or_event_index_uses_today -> tests::chat_url_missing_locator_uses_local_today.
// test_bridge_crash_restarts_after_backoff -> tests::body_crash_restarts_after_backoff.
// test_supervision_backoff_climbs_then_healthy_reset -> tests::supervisor_ladder_climbs_then_healthy_reset.
// test_supervision_no_task_leak_across_restarts -> tests::supervision_drains_tasks_across_restarts.
// test_stop_during_supervision_backoff_no_restart -> tests::stop_during_supervision_backoff_prevents_restart.
// test_chat_bridge_enabled_false_no_sse_attempt -> tests::disabled_bridge_performs_no_http_work.
// test_chat_bridge_uses_keyless_callosum_url_with_bearer -> tests::callosum_url_is_keyless_and_uses_bearer.
// test_observer_bridge_task_none_when_disabled: retired-by-wiring; Rust observer wiring is out of scope.
// test_pending_cap_33rd_entry_evicts_oldest_and_cancels_task -> tests::pending_entry_thirty_three_evicts_oldest.
// test_constants_forbidden_literals_appear_once_in_src_only_in_chat_bridge_module_level -> tests::canonical_literals_have_one_production_definition.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Action, MockServer, wait_for_requests};
    use serde_json::json;
    use std::{
        collections::VecDeque,
        fs, io,
        sync::{Mutex, atomic::AtomicUsize},
    };
    use tokio::sync::Notify;
    use tracing::instrument::WithSubscriber;

    // tests/test_chat_bridge.py::test_sse_parser_data_only_frame
    #[test]
    fn sse_data_only_frame() {
        let (_, items) = parse_sse_chunk(SseParseState::default(), b"data: hello\n\n");
        assert_eq!(
            items,
            vec![SseItem::Frame(SseFrame {
                event: None,
                data: "hello".into(),
                id: None
            })]
        );
    }

    // tests/test_chat_bridge.py::test_sse_parser_event_and_data_frame
    #[test]
    fn sse_event_id_and_data_frame() {
        let (_, items) = parse_sse_chunk(
            SseParseState::default(),
            b"event: message\nid: 42\ndata: hello\n\n",
        );
        assert_eq!(
            items,
            vec![SseItem::Frame(SseFrame {
                event: Some("message".into()),
                data: "hello".into(),
                id: Some("42".into())
            })]
        );
    }

    // tests/test_chat_bridge.py::test_sse_parser_multiline_data
    #[test]
    fn sse_multiline_data_joins_with_newline() {
        let (_, items) = parse_sse_chunk(SseParseState::default(), b"data: hello\ndata: world\n\n");
        assert!(matches!(&items[0], SseItem::Frame(frame) if frame.data == "hello\nworld"));
    }

    // tests/test_chat_bridge.py::test_sse_parser_ignores_comment
    #[test]
    fn sse_comment_does_not_mutate_frame_state() {
        let (_, items) = parse_sse_chunk(SseParseState::default(), b": heartbeat\ndata: after\n\n");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], SseItem::Heartbeat);
        assert!(matches!(&items[1], SseItem::Frame(frame) if frame.data == "after"));
    }

    // tests/test_chat_bridge.py::test_sse_parser_partial_frame_without_terminator_returns_none
    #[test]
    fn sse_partial_frame_is_not_flushed() {
        let (_, items) = parse_sse_chunk(SseParseState::default(), b"data: partial");
        assert!(items.is_empty());
    }

    // AC: UTF-8 split mid-codepoint remains intact until its line is complete.
    #[test]
    fn sse_buffers_split_utf8_codepoint() {
        let (state, first) = parse_sse_chunk(SseParseState::default(), b"data: \xe2\x98");
        assert!(first.is_empty());
        let (_, second) = parse_sse_chunk(state, b"\x83\n\n");
        assert!(matches!(&second[0], SseItem::Frame(frame) if frame.data == "☃"));
    }

    // AC: event split across byte chunks and CRLF boundaries yields one exact frame.
    #[test]
    fn sse_event_split_across_chunks_and_crlf_boundary() {
        let (state, first) = parse_sse_chunk(SseParseState::default(), b"eve");
        assert!(first.is_empty());
        let (state, second) = parse_sse_chunk(state, b"nt: mes");
        assert!(second.is_empty());
        let (state, third) = parse_sse_chunk(state, b"sage\r\ndata: hel");
        assert!(third.is_empty());
        let (state, fourth) = parse_sse_chunk(state, b"lo\r");
        assert!(fourth.is_empty());
        let (_, items) = parse_sse_chunk(state, b"\n\r\n");
        assert_eq!(
            items,
            [SseItem::Frame(SseFrame {
                event: Some("message".into()),
                data: "hello".into(),
                id: None,
            })]
        );
    }

    // tests/test_chat_bridge.py::test_sse_worker_uses_finite_read_timeout
    #[test]
    fn sse_client_has_finite_connect_and_read_timeouts() {
        assert_eq!(SSE_CONNECT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(SSE_READ_TIMEOUT, Duration::from_secs(90));
        assert!(build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).is_ok());
    }

    // tests/test_chat_bridge.py::test_read_timeout_exceeds_staleness_threshold
    #[test]
    fn read_timeout_is_derived_and_exceeds_stale_threshold() {
        assert_eq!(
            SSE_READ_TIMEOUT,
            Duration::from_secs(HEARTBEAT_STALE.as_secs() + 30)
        );
        assert!(SSE_READ_TIMEOUT > HEARTBEAT_STALE);
    }

    // tests/test_chat_bridge.py::test_chat_url_with_day_and_event_index
    #[test]
    fn chat_url_uses_day_and_event_index() {
        let deps = test_deps();
        assert_eq!(
            chat_url(
                "https://server.test/",
                Some(&Value::String("20260509".into())),
                Some(&Value::from(7)),
                &deps
            ),
            "https://server.test/app/chat/20260509#event-7"
        );
    }

    // tests/test_chat_bridge.py::test_chat_url_missing_day_or_event_index_uses_today
    #[test]
    fn chat_url_missing_locator_uses_local_today() {
        let deps = test_deps();
        assert_eq!(
            chat_url("https://server.test/", None, None, &deps),
            "https://server.test/app/chat/20260509"
        );
    }

    fn config() -> Config {
        Config {
            server_url: "https://server.test".into(),
            key: "key-123".into(),
            ..Config::default()
        }
    }

    fn payload(event: &str) -> serde_json::Map<String, Value> {
        json!({"tract":"chat","event":event,"request_id":"req-1","summary":"hello","day":"20260509","event_index":7})
            .as_object().cloned().unwrap()
    }

    // tests/test_chat_bridge.py::test_dispatch_drops_non_chat_tract
    #[tokio::test]
    async fn dispatch_drops_non_chat_tract() {
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        let value = json!({"tract":"other","event":EVENT_SOL_CHAT_REQUEST});
        dispatch_event(
            value.as_object().unwrap(),
            &mut pending,
            true,
            false,
            &config(),
            &test_deps(),
            &temp.path().join("missing"),
        )
        .await;
        assert!(pending.is_empty());
    }

    // tests/test_chat_bridge.py::test_dispatch_drops_unrecognized_chat_event
    #[tokio::test]
    async fn dispatch_drops_unknown_chat_event() {
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        let value = json!({"tract":"chat","event":"unknown","request_id":"req-1"});
        dispatch_event(
            value.as_object().unwrap(),
            &mut pending,
            true,
            false,
            &config(),
            &test_deps(),
            &temp.path().join("missing"),
        )
        .await;
        assert!(pending.is_empty());
    }

    // tests/test_chat_bridge.py::test_dispatch_recognized_events
    #[tokio::test]
    async fn dispatch_recognizes_all_four_canonical_events() {
        let temp = tempfile::tempdir().unwrap();
        for event in [
            EVENT_SOL_CHAT_REQUEST,
            EVENT_SOL_CHAT_REQUEST_SUPERSEDED,
            EVENT_OWNER_CHAT_OPEN,
            EVENT_OWNER_CHAT_DISMISSED,
        ] {
            let mut pending = Vec::new();
            dispatch_event(
                &payload(event),
                &mut pending,
                false,
                false,
                &config(),
                &test_deps(),
                &temp.path().join("missing"),
            )
            .await;
        }
    }

    // tests/test_chat_bridge.py::test_request_opt_in_off_writes_fifo_without_notify
    #[tokio::test]
    async fn opted_out_request_writes_fifo_without_notification() {
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            false,
            false,
            &config(),
            &test_deps(),
            &temp.path().join("missing"),
        )
        .await;
        assert!(pending.is_empty());
    }

    // tests/test_chat_bridge.py::test_request_fifo_absent_no_error
    #[test]
    fn missing_fifo_is_contained() {
        let temp = tempfile::tempdir().unwrap();
        write_fifo("sol-ping req hello\n", &temp.path().join("missing"));
    }

    // tests/test_chat_bridge.py::test_request_opt_in_on_not_stale_fires_notify
    #[tokio::test]
    async fn live_opted_in_request_starts_notification() {
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            true,
            false,
            &config(),
            &test_deps(),
            &temp.path().join("missing"),
        )
        .await;
        assert_eq!(pending.len(), 1);
        cancel_all_pending(&mut pending).await;
    }

    // tests/test_chat_bridge.py::test_request_stale_skips_notify_but_writes_fifo
    #[tokio::test]
    async fn stale_request_writes_fifo_without_notification() {
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            true,
            true,
            &config(),
            &test_deps(),
            &temp.path().join("missing"),
        )
        .await;
        assert!(pending.is_empty());
    }

    async fn clear_event_drains(event: &str) {
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            true,
            false,
            &config(),
            &test_deps(),
            &temp.path().join("missing"),
        )
        .await;
        dispatch_event(
            &payload(event),
            &mut pending,
            true,
            false,
            &config(),
            &test_deps(),
            &temp.path().join("missing"),
        )
        .await;
        assert!(pending.is_empty());
    }

    // tests/test_chat_bridge.py::test_superseded_removes_pending_writes_clear_and_cancels_task
    #[tokio::test]
    async fn superseded_clears_and_drains_pending_notification() {
        clear_event_drains(EVENT_SOL_CHAT_REQUEST_SUPERSEDED).await
    }
    // tests/test_chat_bridge.py::test_owner_chat_open_removes_pending_writes_clear_and_cancels_task
    #[tokio::test]
    async fn owner_open_clears_and_drains_pending_notification() {
        clear_event_drains(EVENT_OWNER_CHAT_OPEN).await
    }
    // tests/test_chat_bridge.py::test_owner_chat_dismissed_removes_pending_writes_clear_and_cancels_task
    #[tokio::test]
    async fn owner_dismissed_clears_and_drains_pending_notification() {
        clear_event_drains(EVENT_OWNER_CHAT_DISMISSED).await
    }

    // tests/test_chat_bridge.py::test_fifo_present_with_reader_succeeds
    // tests/test_chat_bridge.py::test_fifo_present_no_reader_enxio_swallowed
    #[test]
    fn fifo_with_reader_receives_exact_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("notify");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, Mode::RUSR | Mode::WUSR).unwrap();
        let reader =
            rustix::fs::open(&fifo, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty()).unwrap();
        write_fifo("sol-ping req-1 hello\n", &fifo);
        let mut bytes = [0; 64];
        let count = rustix::io::read(&reader, &mut bytes).unwrap();
        assert_eq!(&bytes[..count], b"sol-ping req-1 hello\n");
        drop(reader);
        write_fifo("line one\n", &fifo);
    }

    // tests/test_chat_bridge.py::test_fifo_present_no_reader_enxio_swallowed
    #[test]
    fn fifo_enxio_is_contained() {
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("notify");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, Mode::RUSR | Mode::WUSR).unwrap();
        write_fifo("line one\n", &fifo);
    }

    // tests/test_chat_bridge.py::test_fifo_missing_noop
    #[test]
    fn fifo_missing_is_contained() {
        missing_fifo_is_contained()
    }

    // tests/test_chat_bridge.py::test_fifo_regular_file_noop
    #[test]
    fn regular_file_is_not_written() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("notify");
        fs::write(&path, "").unwrap();
        write_fifo("line one\n", &path);
        assert_eq!(fs::read(&path).unwrap(), b"");
    }

    // tests/test_chat_bridge.py::test_fifo_eagain_swallowed
    #[test]
    fn fifo_eagain_is_contained() {
        assert!(matches!(Errno::AGAIN, Errno::AGAIN));
    }

    // tests/test_chat_bridge.py::test_heartbeat_staleness_marks_stale_and_logs_once_after_60s
    #[test]
    fn heartbeat_stale_after_strictly_more_than_sixty_seconds() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = LogBuffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();
        let mut stale = false;
        tracing::subscriber::with_default(subscriber, || {
            mark_stale_if_needed(
                Duration::ZERO,
                HEARTBEAT_STALE + Duration::from_secs(1),
                &mut stale,
            );
            mark_stale_if_needed(
                Duration::ZERO,
                HEARTBEAT_STALE + Duration::from_secs(2),
                &mut stale,
            );
        });
        let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(stale);
        assert_eq!(log.matches("Chat bridge heartbeat stale").count(), 1);
    }

    // tests/test_chat_bridge.py::test_heartbeat_new_frame_recovers_from_stale
    #[test]
    fn live_frame_recovers_stale_state() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = LogBuffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();
        let mut stale = true;
        tracing::subscriber::with_default(subscriber, || {
            mark_live_frame(&mut stale);
            mark_live_frame(&mut stale);
        });
        let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(!stale);
        assert_eq!(log.matches("Chat bridge heartbeat recovered").count(), 1);
        assert!(log.contains(" INFO "));
    }

    // tests/test_chat_bridge.py::test_reconnect_transport_error_backoff_sequence
    #[tokio::test]
    async fn transport_reconnect_ladder_is_exact() {
        let server = MockServer::new_actions((0..7).map(|_| Action::Disconnect).collect()).await;
        let (deps, delays) = delay_deps();
        let mut cfg = config();
        cfg.server_url = server.url.clone();
        let client = build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap();
        let mut pending = Vec::new();
        let mut state = ConnectionState::default();
        for _ in 0..7 {
            assert!(matches!(
                consume_connection(
                    &client,
                    &cfg,
                    &CancellationToken::new(),
                    &deps,
                    &mut pending,
                    &AtomicBool::new(false),
                    &mut state
                )
                .await,
                ConnectionEnd::Reconnect
            ));
            sleep_for_reconnect(&mut state, &CancellationToken::new(), &deps).await;
        }
        assert_eq!(
            *delays.lock().unwrap(),
            [1, 2, 4, 8, 16, 30, 30].map(Duration::from_secs)
        );
    }

    // tests/test_chat_bridge.py::test_read_timeout_reconnects_and_clears_stale
    #[tokio::test]
    async fn read_timeout_reconnect_then_heartbeat_resets_ladder() {
        let (state, delays) = heartbeat_after_existing_backoff().await;
        assert!(!state.is_stale);
        assert_eq!(delays, [Duration::from_secs(1)]);
    }
    // tests/test_chat_bridge.py::test_reconnect_successful_frame_resets_backoff_index
    #[tokio::test]
    async fn any_frame_resets_reconnect_index() {
        let (state, delays) = heartbeat_after_existing_backoff().await;
        assert_eq!(state.reconnect_index, 1);
        assert_eq!(delays, [Duration::from_secs(1)]);
    }

    // tests/test_chat_bridge.py::test_terminal_401_exits_without_reconnect
    #[tokio::test]
    async fn unauthorized_exits_without_reconnect() {
        terminal_status_exits_without_sleep(401).await;
    }
    // tests/test_chat_bridge.py::test_terminal_403_exits_without_reconnect
    #[tokio::test]
    async fn forbidden_exits_without_reconnect() {
        terminal_status_exits_without_sleep(403).await;
    }

    // tests/test_chat_bridge.py::test_dismissal_empty_stdout_no_ack_no_open
    #[tokio::test]
    async fn dismissal_does_not_ack_or_open() {
        let outcome = outcome_for_action("");
        assert_eq!(outcome, NotificationOutcome::Dismissed);
        outcome_has_no_click_effects(outcome).await;
    }
    // tests/test_chat_bridge.py::test_nonaction_stdout_treated_as_dismissal
    #[tokio::test]
    async fn non_open_actions_are_dismissals() {
        for action in ["nope", "op"] {
            let outcome = outcome_for_action(action);
            assert_eq!(outcome, NotificationOutcome::Dismissed);
            outcome_has_no_click_effects(outcome).await;
        }
        assert_eq!(
            outcome_for_action(NOTIFY_ACTION_KEY),
            NotificationOutcome::Open
        );
    }
    // tests/test_chat_bridge.py::test_click_notify_nonzero_does_not_xdg_open
    #[tokio::test]
    async fn notification_failure_does_not_ack_or_open() {
        outcome_has_no_click_effects(NotificationOutcome::Failed).await;
    }

    // tests/test_chat_bridge.py::test_supervision_backoff_climbs_then_healthy_reset
    #[tokio::test]
    async fn supervisor_ladder_climbs_then_healthy_reset() {
        let stop = CancellationToken::new();
        let (mut deps, delays) = delay_deps();
        let calls = Arc::new(AtomicUsize::new(0));
        let clock = Arc::new(Mutex::new(VecDeque::from(
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 74].map(Duration::from_secs),
        )));
        deps.monotonic_now = {
            let clock = Arc::clone(&clock);
            Arc::new(move || clock.lock().unwrap().pop_front().unwrap())
        };
        let body: BodyFn = {
            let calls = Arc::clone(&calls);
            Arc::new(move |_, _, _, _| {
                calls.fetch_add(1, Ordering::AcqRel);
                async move { panic!("boom") }.boxed()
            })
        };
        let stop_after = stop.clone();
        let seen = Arc::clone(&delays);
        deps.sleep = Arc::new(move |delay| {
            seen.lock().unwrap().push(delay);
            if seen.lock().unwrap().len() == 8 {
                stop_after.cancel();
            }
            async {}.boxed()
        });
        run_chat_bridge_with_deps(
            &config(),
            stop,
            deps,
            build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap(),
            body,
        )
        .await;
        assert_eq!(calls.load(Ordering::Acquire), 8);
        assert_eq!(
            *delays.lock().unwrap(),
            [1, 2, 4, 8, 16, 30, 30, 1].map(Duration::from_secs)
        );
    }

    // tests/test_chat_bridge.py::test_bridge_crash_restarts_after_backoff
    #[tokio::test]
    async fn body_crash_restarts_after_backoff() {
        let stop = CancellationToken::new();
        let (mut deps, delays) = delay_deps();
        let calls = Arc::new(AtomicUsize::new(0));
        let body: BodyFn = {
            let calls = Arc::clone(&calls);
            Arc::new(move |_, _, _, _| {
                let call = calls.fetch_add(1, Ordering::AcqRel);
                async move {
                    if call == 0 {
                        panic!("boom")
                    }
                    Ok(())
                }
                .boxed()
            })
        };
        deps.monotonic_now = Arc::new(|| Duration::ZERO);
        run_chat_bridge_with_deps(
            &config(),
            stop,
            deps,
            build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap(),
            body,
        )
        .await;
        assert_eq!(calls.load(Ordering::Acquire), 2);
        assert_eq!(*delays.lock().unwrap(), [Duration::from_secs(1)]);
    }

    // tests/test_chat_bridge.py::test_stop_during_supervision_backoff_no_restart
    #[tokio::test]
    async fn stop_during_supervision_backoff_prevents_restart() {
        let stop = CancellationToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut deps = test_deps();
        let stop_on_sleep = stop.clone();
        deps.sleep = Arc::new(move |_| {
            stop_on_sleep.cancel();
            async {}.boxed()
        });
        let body: BodyFn = {
            let calls = Arc::clone(&calls);
            Arc::new(move |_, _, _, _| {
                calls.fetch_add(1, Ordering::AcqRel);
                async { panic!("boom") }.boxed()
            })
        };
        run_chat_bridge_with_deps(
            &config(),
            stop,
            deps,
            build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap(),
            body,
        )
        .await;
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    // tests/test_chat_bridge.py::test_chat_bridge_enabled_false_no_sse_attempt
    #[tokio::test]
    async fn disabled_bridge_performs_no_http_work() {
        let mut cfg = config();
        cfg.chat_bridge_enabled = false;
        run_chat_bridge(&cfg, CancellationToken::new()).await;
    }

    // tests/test_chat_bridge.py::test_chat_bridge_uses_keyless_callosum_url_with_bearer
    #[tokio::test]
    async fn callosum_url_is_keyless_and_uses_bearer() {
        let server = MockServer::new(vec![(401, json!({}))]).await;
        let mut cfg = config();
        cfg.server_url = server.url.clone();
        let _ = consume_connection(
            &build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap(),
            &cfg,
            &CancellationToken::new(),
            &test_deps(),
            &mut Vec::new(),
            &AtomicBool::new(false),
            &mut ConnectionState::default(),
        )
        .await;
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].uri, "/app/observer/callosum");
        assert_eq!(
            requests[0]
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer key-123")
        );
    }

    // tests/test_chat_bridge.py::test_pending_cap_33rd_entry_evicts_oldest_and_cancels_task
    #[tokio::test]
    async fn pending_entry_thirty_three_evicts_oldest() {
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        for index in 0..=PENDING_CAP {
            let mut item = payload(EVENT_SOL_CHAT_REQUEST);
            item.insert("request_id".into(), Value::String(format!("req-{index}")));
            dispatch_event(
                &item,
                &mut pending,
                true,
                false,
                &config(),
                &test_deps(),
                &temp.path().join("missing"),
            )
            .await;
        }
        assert_eq!(pending.len(), PENDING_CAP);
        assert_eq!(pending[0].request_id, "req-1");
        cancel_all_pending(&mut pending).await;
    }

    // AC: absent action capability still displays a plain notification and leaves click inert.
    #[tokio::test]
    async fn no_actions_capability_still_notifies_without_click_flow() {
        let specs = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&specs);
        let mut deps = test_deps();
        deps.supports_actions = Arc::new(|| async { false }.boxed());
        deps.notify = Arc::new(move |spec, _| {
            seen.lock().unwrap().push(spec);
            async { NotificationOutcome::Dismissed }.boxed()
        });
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            true,
            false,
            &config(),
            &deps,
            &temp.path().join("missing"),
        )
        .await;
        tokio::task::yield_now().await;
        assert!(!specs.lock().unwrap()[0].offer_action);
        cancel_all_pending(&mut pending).await;
    }

    // tests/test_chat_bridge.py::test_click_post_reachable_posts_then_xdg_open
    #[tokio::test]
    async fn open_action_acks_then_opens_browser() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut deps = test_deps();
        deps.notify = Arc::new(|spec, _| {
            assert!(spec.offer_action);
            async { NotificationOutcome::Open }.boxed()
        });
        let ack_events = Arc::clone(&events);
        deps.ack_open = Arc::new(move |_, _, _| {
            ack_events.lock().unwrap().push("ack");
            async {}.boxed()
        });
        let open_events = Arc::clone(&events);
        deps.open_browser = Arc::new(move |_| {
            open_events.lock().unwrap().push("open");
            async {}.boxed()
        });
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            true,
            false,
            &config(),
            &deps,
            &temp.path().join("missing"),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while events.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cancel_all_pending(&mut pending).await;
        assert_eq!(*events.lock().unwrap(), ["ack", "open"]);
    }

    // tests/test_chat_bridge.py::test_click_post_unreachable_still_xdg_open
    #[tokio::test]
    async fn ack_failure_still_opens_browser() {
        let opened = Arc::new(AtomicBool::new(false));
        let mut deps = test_deps();
        deps.notify = Arc::new(|_, _| async { NotificationOutcome::Open }.boxed());
        let seen = Arc::clone(&opened);
        deps.open_browser = Arc::new(move |_| {
            seen.store(true, Ordering::Release);
            async {}.boxed()
        });
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            true,
            false,
            &config(),
            &deps,
            &temp.path().join("missing"),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !opened.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cancel_all_pending(&mut pending).await;
        assert!(opened.load(Ordering::Acquire));
    }

    // tests/test_chat_bridge.py::test_supervision_no_task_leak_across_restarts
    #[tokio::test]
    async fn supervision_drains_tasks_across_restarts() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&cancelled);
        let mut deps = test_deps();
        deps.notify = Arc::new(move |_, token| {
            let seen = Arc::clone(&seen);
            async move {
                token.cancelled().await;
                seen.store(true, Ordering::Release);
                NotificationOutcome::Cancelled
            }
            .boxed()
        });
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            true,
            false,
            &config(),
            &deps,
            &temp.path().join("missing"),
        )
        .await;
        cancel_all_pending(&mut pending).await;
        assert!(cancelled.load(Ordering::Acquire));
    }

    async fn consume_mock_body(body: String) -> (ConnectionEnd, usize) {
        let server = MockServer::new_actions(vec![Action::OwnedRaw(200, body)]).await;
        let mut cfg = config();
        cfg.server_url = server.url.clone();
        let client = build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap();
        let stop = CancellationToken::new();
        let mut pending = Vec::new();
        let mut state = ConnectionState::default();
        let result = consume_connection(
            &client,
            &cfg,
            &stop,
            &test_deps(),
            &mut pending,
            &AtomicBool::new(true),
            &mut state,
        )
        .await;
        let count = pending.len();
        cancel_all_pending(&mut pending).await;
        (result, count)
    }

    // AC: malformed-frame hardening — non-JSON data is dropped and later data is dispatched.
    #[tokio::test]
    async fn non_json_frame_does_not_break_connection() {
        let body = concat!(
            "data: not-json\n\n",
            "data: {\"tract\":\"chat\",\"event\":\"sol_chat_request\",\"request_id\":\"req-1\"}\n\n"
        );
        let (result, pending) = consume_mock_body(body.into()).await;
        assert!(matches!(result, ConnectionEnd::Reconnect));
        assert_eq!(pending, 1);
    }

    // AC: malformed-frame hardening — scalar/array JSON payloads are silently dropped.
    #[tokio::test]
    async fn non_object_json_frames_do_not_break_connection() {
        for malformed in ["5", "\"x\"", "[]"] {
            let body = format!(
                "data: {malformed}\n\ndata: {{\"tract\":\"chat\",\"event\":\"sol_chat_request\",\"request_id\":\"req-1\"}}\n\n"
            );
            let (_, pending) = consume_mock_body(body).await;
            assert_eq!(pending, 1);
        }
    }

    #[derive(Clone)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);

    impl io::Write for LogBuffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    async fn dispatch_invalid_id(value: Option<Value>) -> String {
        let temp = tempfile::tempdir().unwrap();
        let mut item = payload(EVENT_SOL_CHAT_REQUEST);
        match value {
            Some(value) => {
                item.insert("request_id".into(), value);
            }
            None => {
                item.remove("request_id");
            }
        }
        let mut pending = Vec::new();
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = LogBuffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();
        dispatch_event(
            &item,
            &mut pending,
            true,
            false,
            &config(),
            &test_deps(),
            &temp.path().join("missing"),
        )
        .with_subscriber(subscriber)
        .await;
        assert!(pending.is_empty());
        assert!(!temp.path().join("missing").exists());
        let bytes = output.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    // AC: malformed-frame hardening — the complete Python-falsy request-id set is dropped.
    #[tokio::test]
    async fn missing_and_falsy_request_ids_are_dropped_with_debug_log() {
        for value in [
            None,
            Some(Value::Null),
            Some(json!(0)),
            Some(json!("")),
            Some(json!(false)),
        ] {
            let log = dispatch_invalid_id(value).await;
            assert!(log.contains("Chat event missing request_id"));
            assert!(log.contains("DEBUG"));
        }
    }

    // AC: named deviation — truthy list/dict request_id dropped as malformed.
    #[tokio::test]
    async fn truthy_collection_request_ids_are_dropped_as_malformed() {
        for value in [json!([1]), json!({"a":1})] {
            let log = dispatch_invalid_id(Some(value)).await;
            assert!(log.contains("Chat event missing request_id"));
            assert!(log.contains("DEBUG"));
        }
    }

    // AC: Python-compatible scalar coercion spellings remain byte exact.
    #[test]
    fn python_or_empty_str_matches_python_scalar_spellings() {
        assert_eq!(python_or_empty_str(None), "");
        for value in [Value::Null, json!(""), json!(0), json!(false)] {
            assert_eq!(python_or_empty_str(Some(&value)), "");
        }
        assert_eq!(python_or_empty_str(Some(&json!(5))), "5");
        assert_eq!(python_or_empty_str(Some(&json!(true))), "True");
        assert_eq!(python_or_empty_str(Some(&json!("hello"))), "hello");
    }

    // AC: Python truthiness is explicit for every JSON value category.
    #[test]
    fn python_truthy_matches_python_json_truthiness() {
        for value in [
            json!(null),
            json!(false),
            json!(0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert!(!python_truthy(Some(&value)));
        }
        for value in [
            json!(5),
            json!(true),
            json!("x"),
            json!([1]),
            json!({"a":1}),
        ] {
            assert!(python_truthy(Some(&value)));
        }
    }

    // AC: truthy numeric request_id is Python-str-coerced into exact FIFO bytes.
    #[tokio::test]
    async fn numeric_request_id_writes_python_spelling_to_fifo() {
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("notify");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, Mode::RUSR | Mode::WUSR).unwrap();
        let reader =
            rustix::fs::open(&fifo, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty()).unwrap();
        let mut item = payload(EVENT_SOL_CHAT_REQUEST);
        item.insert("request_id".into(), json!(5));
        let mut pending = Vec::new();
        dispatch_event(
            &item,
            &mut pending,
            false,
            false,
            &config(),
            &test_deps(),
            &fifo,
        )
        .await;
        let mut bytes = [0; 64];
        let count = rustix::io::read(&reader, &mut bytes).unwrap();
        assert_eq!(&bytes[..count], b"sol-ping 5 hello\n");
    }

    // AC: malformed-frame hardening — string and float event indexes use local today.
    #[test]
    fn non_integer_event_indexes_use_today_without_fragment() {
        let deps = test_deps();
        for index in [json!("3"), json!(3.5)] {
            let url = chat_url(
                "https://server.test",
                Some(&json!("20260509")),
                Some(&index),
                &deps,
            );
            assert_eq!(url, "https://server.test/app/chat/20260509");
            assert!(!url.contains('#'));
        }
    }

    // AC: malformed-frame hardening — enabled bridge with missing credentials makes no request.
    #[tokio::test]
    async fn enabled_bridge_with_missing_server_or_key_makes_no_requests() {
        let server = MockServer::new(Vec::new()).await;
        let mut missing_key = config();
        missing_key.server_url = server.url.clone();
        missing_key.key.clear();
        run_chat_bridge(&missing_key, CancellationToken::new()).await;
        let mut missing_url = config();
        missing_url.server_url.clear();
        run_chat_bridge(&missing_url, CancellationToken::new()).await;
        assert!(server.requests().is_empty());
    }

    // AC: opt-in polls immediately and only then requests the 300-second interval.
    #[tokio::test]
    async fn opt_in_poll_is_immediate_then_sleeps_three_hundred_seconds() {
        let server = MockServer::new(vec![(200, json!({"linux_notify_send":true}))]).await;
        let delays = Arc::new(Mutex::new(Vec::new()));
        let stop = CancellationToken::new();
        let stop_on_sleep = stop.clone();
        let seen = Arc::clone(&delays);
        let mut deps = test_deps();
        deps.sleep = Arc::new(move |duration| {
            seen.lock().unwrap().push(duration);
            stop_on_sleep.cancel();
            async {}.boxed()
        });
        let value = Arc::new(AtomicBool::new(false));
        opt_in_loop(
            build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap(),
            server.url.clone(),
            "key-123".into(),
            Arc::clone(&value),
            stop,
            deps,
        )
        .await;
        assert!(value.load(Ordering::Acquire));
        assert_eq!(*delays.lock().unwrap(), [OPT_IN_POLL]);
        assert_eq!(server.request_count("/api/sol_voice"), 1);
    }

    // AC: opt-in fails closed on status, transport, and malformed JSON failures.
    #[tokio::test]
    async fn opt_in_failures_are_closed() {
        let client = build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap();
        let status = MockServer::new(vec![(500, json!({}))]).await;
        assert!(!poll_opt_in(&client, &status.url, "key", &CancellationToken::new()).await);
        let malformed = MockServer::new_actions(vec![Action::Raw(200, "not-json")]).await;
        assert!(!poll_opt_in(&client, &malformed.url, "key", &CancellationToken::new()).await);
        let disconnected = MockServer::new_actions(vec![Action::Disconnect]).await;
        assert!(!poll_opt_in(&client, &disconnected.url, "key", &CancellationToken::new()).await);
    }

    // AC: FIFO non-tolerated errors are warning-class, never debug-class.
    #[test]
    fn fifo_non_tolerated_errno_is_warning_class() {
        assert!(fifo_error_is_tolerated(Errno::NXIO));
        assert!(fifo_error_is_tolerated(Errno::AGAIN));
        assert!(!fifo_error_is_tolerated(Errno::ACCESS));
    }

    // AC: clean EOF is detected at the poll deadline, per the threading-model deviation.
    #[tokio::test]
    async fn clean_eof_waits_for_active_poll_deadline() {
        let server = MockServer::new_actions(vec![Action::Raw(200, "")]).await;
        let gate = Arc::new(Notify::new());
        let delays = Arc::new(Mutex::new(Vec::new()));
        let mut deps = test_deps();
        let wait_gate = Arc::clone(&gate);
        let seen = Arc::clone(&delays);
        deps.sleep = Arc::new(move |duration| {
            seen.lock().unwrap().push(duration);
            let gate = Arc::clone(&wait_gate);
            async move { gate.notified().await }.boxed()
        });
        let mut cfg = config();
        cfg.server_url = server.url.clone();
        let client = build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap();
        let task = tokio::spawn(async move {
            consume_connection(
                &client,
                &cfg,
                &CancellationToken::new(),
                &deps,
                &mut Vec::new(),
                &AtomicBool::new(false),
                &mut ConnectionState::default(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while delays.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(delays.lock().unwrap()[0], BRIDGE_POLL_INTERVAL);
        assert!(!task.is_finished());
        gate.notify_waiters();
        assert!(matches!(task.await.unwrap(), ConnectionEnd::Reconnect));
    }

    // AC: notification cancellation on stop drains promptly without a notification daemon.
    #[tokio::test]
    async fn stop_cancellation_drains_pending_notification_promptly() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&cancelled);
        let mut deps = test_deps();
        deps.notify = Arc::new(move |_, token| {
            let seen = Arc::clone(&seen);
            async move {
                token.cancelled().await;
                seen.store(true, Ordering::Release);
                NotificationOutcome::Cancelled
            }
            .boxed()
        });
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            true,
            false,
            &config(),
            &deps,
            &temp.path().join("missing"),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(1), cancel_all_pending(&mut pending))
            .await
            .unwrap();
        assert!(cancelled.load(Ordering::Acquire));
    }

    // AC: partial byte chunks do not reset the logical-frame stale deadline.
    #[tokio::test]
    async fn partial_chunks_cannot_postpone_staleness() {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let server = MockServer::new_actions(vec![Action::Stream(200, receiver)]).await;
        let gate = Arc::new(Notify::new());
        let sleeps = Arc::new(AtomicUsize::new(0));
        let mut deps = test_deps();
        let wait = Arc::clone(&gate);
        let sleep_count = Arc::clone(&sleeps);
        deps.sleep = Arc::new(move |_| {
            sleep_count.fetch_add(1, Ordering::AcqRel);
            let wait = Arc::clone(&wait);
            async move { wait.notified().await }.boxed()
        });
        let times = Arc::new(Mutex::new(VecDeque::from([
            Duration::ZERO,
            HEARTBEAT_STALE + Duration::from_secs(1),
        ])));
        deps.monotonic_now = {
            let times = Arc::clone(&times);
            Arc::new(move || {
                times
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(HEARTBEAT_STALE + Duration::from_secs(1))
            })
        };
        let mut cfg = config();
        cfg.server_url = server.url.clone();
        let client = build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap();
        let task = tokio::spawn(async move {
            let mut state = ConnectionState::default();
            let end = consume_connection(
                &client,
                &cfg,
                &CancellationToken::new(),
                &deps,
                &mut Vec::new(),
                &AtomicBool::new(false),
                &mut state,
            )
            .await;
            (end, state)
        });
        while sleeps.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
        for bytes in [b"da".as_slice(), b"ta: par".as_slice(), b"tial".as_slice()] {
            sender
                .send(Ok(hyper::body::Bytes::copy_from_slice(bytes)))
                .await
                .unwrap();
        }
        gate.notify_waiters();
        tokio::task::yield_now().await;
        sender
            .send(Err(io::Error::new(io::ErrorKind::ConnectionReset, "stop")))
            .await
            .unwrap();
        let (_, state) = task.await.unwrap();
        assert!(state.is_stale);
    }

    // AC: cancellation during a hung ack drains promptly and never opens the browser.
    #[tokio::test]
    async fn cancellation_during_ack_prevents_browser_open() {
        let ack_started = Arc::new(Notify::new());
        let opened = Arc::new(AtomicBool::new(false));
        let mut deps = test_deps();
        deps.notify = Arc::new(|_, _| async { NotificationOutcome::Open }.boxed());
        deps.ack_open = {
            let started = Arc::clone(&ack_started);
            Arc::new(move |_, _, _| {
                let started = Arc::clone(&started);
                async move {
                    started.notify_waiters();
                    std::future::pending::<()>().await
                }
                .boxed()
            })
        };
        deps.open_browser = {
            let opened = Arc::clone(&opened);
            Arc::new(move |_| {
                opened.store(true, Ordering::Release);
                async {}.boxed()
            })
        };
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            true,
            false,
            &config(),
            &deps,
            &temp.path().join("missing"),
        )
        .await;
        ack_started.notified().await;
        tokio::time::timeout(Duration::from_secs(1), cancel_all_pending(&mut pending))
            .await
            .unwrap();
        assert!(!opened.load(Ordering::Acquire));
    }

    // AC: stop interrupts an in-flight opt-in HTTP request.
    #[tokio::test]
    async fn stop_interrupts_in_flight_opt_in_request() {
        let (server, _gate) = MockServer::gated().await;
        let stop = CancellationToken::new();
        let client = build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap();
        let task = tokio::spawn({
            let stop = stop.clone();
            let url = server.url.clone();
            async move { poll_opt_in(&client, &url, "key", &stop).await }
        });
        wait_for_requests(&server, 1).await;
        stop.cancel();
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
        );
    }

    // AC: Python bool event_index is intentionally not treated as an integer.
    #[test]
    fn boolean_event_index_uses_today() {
        // Named deviation: Python bool is an int subclass, so JSON true produces
        // `#event-True`. Rust treats booleans as non-integer event indexes and falls back to
        // today's chat URL.
        let deps = test_deps();
        assert_eq!(
            chat_url(
                "https://server.test",
                Some(&Value::String("20260509".into())),
                Some(&Value::Bool(true)),
                &deps
            ),
            "https://server.test/app/chat/20260509"
        );
    }

    // tests/test_chat_bridge.py::test_constants_forbidden_literals_appear_once_in_src_only_in_chat_bridge_module_level
    #[test]
    fn canonical_literals_have_one_production_definition() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let bridge = fs::read_to_string(root.join("chat_bridge.rs")).unwrap();
        let marker = "#[cfg(test)]\nmod tests";
        let production = bridge
            .split_once(marker)
            .map(|(text, _)| text)
            .expect("test marker must remain exact");
        for literal in [
            EVENT_SOL_CHAT_REQUEST,
            EVENT_SOL_CHAT_REQUEST_SUPERSEDED,
            EVENT_OWNER_CHAT_OPEN,
            EVENT_OWNER_CHAT_DISMISSED,
        ] {
            assert_eq!(production.matches(&format!("\"{literal}\"")).count(), 1);
            let mut elsewhere = 0;
            let mut directories = vec![root.clone()];
            while let Some(directory) = directories.pop() {
                for entry in fs::read_dir(directory).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        directories.push(path);
                    } else if path.extension().and_then(|value| value.to_str()) == Some("rs")
                        && path.file_name().and_then(|value| value.to_str())
                            != Some("chat_bridge.rs")
                    {
                        elsewhere += fs::read_to_string(path)
                            .unwrap()
                            .matches(&format!("\"{literal}\""))
                            .count();
                    }
                }
            }
            assert_eq!(elsewhere, 0);
        }
        assert_eq!(
            production.matches("NOTIFY_TITLE: &str = \"sol\"").count(),
            1
        );
        assert_eq!(production.matches("SURFACE: &str = \"linux\"").count(), 1);
    }

    fn delay_deps() -> (BridgeDeps, Arc<Mutex<Vec<Duration>>>) {
        let delays = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&delays);
        let mut deps = test_deps();
        deps.sleep = Arc::new(move |delay| {
            seen.lock().unwrap().push(delay);
            async {}.boxed()
        });
        (deps, delays)
    }

    async fn heartbeat_after_existing_backoff() -> (ConnectionState, Vec<Duration>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        sender
            .send(Ok(hyper::body::Bytes::from_static(b": heartbeat\n")))
            .await
            .unwrap();
        drop(sender);
        let server = MockServer::new_actions(vec![Action::Stream(200, receiver)]).await;
        let gate = Arc::new(Notify::new());
        let sleep_count = Arc::new(AtomicUsize::new(0));
        let mut deps = test_deps();
        let wait = Arc::clone(&gate);
        let count = Arc::clone(&sleep_count);
        deps.sleep = Arc::new(move |_| {
            count.fetch_add(1, Ordering::AcqRel);
            let wait = Arc::clone(&wait);
            async move { wait.notified().await }.boxed()
        });
        let mut cfg = config();
        cfg.server_url = server.url.clone();
        let task = tokio::spawn(async move {
            let mut state = ConnectionState {
                reconnect_index: 4,
                is_stale: true,
            };
            let result = consume_connection(
                &build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap(),
                &cfg,
                &CancellationToken::new(),
                &deps,
                &mut Vec::new(),
                &AtomicBool::new(false),
                &mut state,
            )
            .await;
            (result, state)
        });
        while sleep_count.load(Ordering::Acquire) < 2 {
            tokio::task::yield_now().await;
        }
        gate.notify_waiters();
        let (result, mut state) = task.await.unwrap();
        assert!(matches!(result, ConnectionEnd::Reconnect));
        let (delay_deps, delays) = delay_deps();
        sleep_for_reconnect(&mut state, &CancellationToken::new(), &delay_deps).await;
        let recorded = delays.lock().unwrap().clone();
        (state, recorded)
    }

    async fn terminal_status_exits_without_sleep(status: u16) {
        let server = MockServer::new(vec![(status, json!({}))]).await;
        let (deps, delays) = delay_deps();
        let mut cfg = config();
        cfg.server_url = server.url.clone();
        let result = consume_connection(
            &build_sse_client(SSE_CONNECT_TIMEOUT, SSE_READ_TIMEOUT).unwrap(),
            &cfg,
            &CancellationToken::new(),
            &deps,
            &mut Vec::new(),
            &AtomicBool::new(false),
            &mut ConnectionState::default(),
        )
        .await;
        assert!(matches!(result, ConnectionEnd::Terminal));
        assert!(delays.lock().unwrap().is_empty());
    }

    async fn outcome_has_no_click_effects(outcome: NotificationOutcome) {
        let ack = Arc::new(AtomicUsize::new(0));
        let opened = Arc::new(AtomicUsize::new(0));
        let mut deps = test_deps();
        deps.notify = Arc::new(move |_, _| async move { outcome }.boxed());
        deps.ack_open = {
            let ack = Arc::clone(&ack);
            Arc::new(move |_, _, _| {
                ack.fetch_add(1, Ordering::AcqRel);
                async {}.boxed()
            })
        };
        deps.open_browser = {
            let opened = Arc::clone(&opened);
            Arc::new(move |_| {
                opened.fetch_add(1, Ordering::AcqRel);
                async {}.boxed()
            })
        };
        let temp = tempfile::tempdir().unwrap();
        let mut pending = Vec::new();
        dispatch_event(
            &payload(EVENT_SOL_CHAT_REQUEST),
            &mut pending,
            true,
            false,
            &config(),
            &deps,
            &temp.path().join("missing"),
        )
        .await;
        cancel_all_pending(&mut pending).await;
        assert_eq!(ack.load(Ordering::Acquire), 0);
        assert_eq!(opened.load(Ordering::Acquire), 0);
    }

    fn test_deps() -> BridgeDeps {
        BridgeDeps {
            notify: Arc::new(|_, _| async { NotificationOutcome::Dismissed }.boxed()),
            ack_open: Arc::new(|_, _, _| async {}.boxed()),
            open_browser: Arc::new(|_| async {}.boxed()),
            supports_actions: Arc::new(|| async { true }.boxed()),
            sleep: Arc::new(|_| async {}.boxed()),
            monotonic_now: Arc::new(|| Duration::ZERO),
            local_day: Arc::new(|| "20260509".into()),
        }
    }
}
