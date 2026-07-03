# Chat Bridge Resilience

Planning note for the desktop notification bridge reliability fixes. This records the implementation decisions only; source changes are still pending.

## Scope

Fix three coupled defects in `src/solstone_linux/chat_bridge.py`:

- Silent SSE connection death must reconnect instead of leaving the worker blocked forever.
- Desktop notification dismissal must not be treated as an `open` action.
- An unexpected bridge crash must restart the bridge without accumulating worker, opt-in, or notification tasks.

Out of scope: `_SseParser`, `EVENT_*` constants, `_HANDLED_EVENTS`, FIFO mirroring, `PENDING_CAP` and `_enforce_pending_cap`, opt-in polling semantics, observer status events, config knobs, TCP keepalive, pooling, transport rewrites, CHANGELOG entries, and version bumps.

## Decision 1: Finite SSE Read Timeout

Add module-level timeout constants near `HEARTBEAT_STALE_SECONDS`:

- `SSE_CONNECT_TIMEOUT_SECONDS = 10`
- `SSE_READ_TIMEOUT_SECONDS = HEARTBEAT_STALE_SECONDS + 30`

The read timeout is deliberately derived from `HEARTBEAT_STALE_SECONDS`, not duplicated as `90`, so the invariant cannot silently invert. A quiet-but-alive stream should first pass the soft-stale window and suppress notifications; only after that should the read be force-reconnected.

Replace the SSE request timeout tuple with `(SSE_CONNECT_TIMEOUT_SECONDS, SSE_READ_TIMEOUT_SECONDS)`. The infinite-read tuple `(10, None)` must no longer exist in this call path.

Current-code confirmation:

- `HEARTBEAT_STALE_SECONDS` is currently `60` at `chat_bridge.py:52`.
- `_sse_worker` currently uses `timeout=(10, None)` at `chat_bridge.py:151-156`.
- `requests.exceptions.ReadTimeout` is a subclass of `requests.exceptions.RequestException` in the project venv.
- `requests.get` and `response.iter_lines` are inside the same `try`, and the only catch is `except requests.RequestException` at `chat_bridge.py:184-185`.
- The pushed `_transport_error` frame is already consumed by `run_chat_bridge` at `chat_bridge.py:440-445`, setting `reconnect = True`. No new reconnect wiring is needed.

## Decision 2: Exact Notification Action Gate

Add one module-level action-key constant:

- `NOTIFY_ACTION_KEY = "open"`

Use that constant in both places that must stay aligned:

- Build the `notify-send` action argument as `--action=<NOTIFY_ACTION_KEY>=Open`.
- After `proc.communicate()`, decode stdout and exact-match the stripped action value against `NOTIFY_ACTION_KEY`.

Keep the current cancellation handling and the current nonzero-returncode early return. Only an exact action-key match may run the open-ack POST and `xdg-open`. Empty stdout, garbage stdout, or partial stdout is a dismissal and must not ack or open. This is an exact match, not a substring check.

Current-code confirmation:

- `_handle_one_notification` currently passes the literal `--action=open=Open` at `chat_bridge.py:223`.
- It currently discards `stdout` from `proc.communicate()` at `chat_bridge.py:230`.
- It currently gates ack/open only on `proc.returncode == 0` at `chat_bridge.py:240-264`.
- `FakeProc.communicate` currently always returns empty stdout in `tests/test_chat_bridge.py:54-55`, so the fixture must be parameterized.

## Decision 3: Supervisor Around a Restartable Body

Split `run_chat_bridge` into two module-level coroutines.

`_run_bridge_body(config, stop_event)` owns the current bridge incarnation: server URL/key/SSE setup, pending request state, opt-in task, reconnect index, staleness state, worker task, thread stop event, the outer reconnect loop, the inner frame loop, and the cleanup `finally`.

`run_chat_bridge(config, stop_event)` becomes the supervisor. It keeps the one-time config gates for `chat_bridge_enabled`, `server_url`, and `key`, then repeatedly runs `_run_bridge_body`. A normal body return is terminal and the supervisor returns. An `Exception` is logged as a bridge crash with `exc_info=True`, then the supervisor backs off and restarts unless `stop_event` has been set.

Add `HEALTHY_RUN_SECONDS = 60`. If a body ran at least this long before crashing, reset the supervisor backoff index to zero. Rapid consecutive crashes climb the existing `RECONNECT_DELAYS` ladder and cap at 30 seconds.

Required properties:

- Do not add an explicit `except asyncio.CancelledError`. The project requires Python 3.10+, where `asyncio.CancelledError` derives from `BaseException`; `except Exception` will not catch observer teardown cancellation. This matches the existing sync-service pattern at `sync.py:563-565`, which also catches only `Exception`.
- Normal body return remains terminal. Stop-event shutdown exits the body normally after its cleanup; 401/403 authorization failure still sets `thread_stop` and returns normally. The supervisor must not restart either path.
- Restart backoff reuses `RECONNECT_DELAYS` and `_sleep_reconnect(delay, stop_event)`, so existing tests can keep driving sleeps by patching `chat_bridge.asyncio.sleep`.
- `_run_bridge_body` must be module-level and patchable as `chat_bridge._run_bridge_body` for deterministic supervisor tests.
- The body `finally` must run on every incarnation so opt-in tasks, pending notification tasks, and worker threads do not accumulate across restarts.

Current-code confirmation:

- The one-time config gates are currently at `chat_bridge.py:387-391`.
- The state and loop body to extract begins at `chat_bridge.py:393` and runs through the cleanup at `chat_bridge.py:491`.
- The current crash behavior is only logging and return at `chat_bridge.py:492-493`.
- Existing reconnect delay constants and stop-aware sleep already exist at `chat_bridge.py:51` and `chat_bridge.py:380-382`.

## Test Plan

Change existing tests:

- `FakeProc`: add stdout bytes support. Acceptance: click/dismissal tests can model `notify-send --wait` stdout accurately.
- `test_click_post_reachable_posts_then_xdg_open`: make the fake process return the exact action key. Acceptance: exact click still posts and opens.
- `test_click_post_unreachable_still_xdg_open`: make the fake process return the exact action key. Acceptance: ack failure still does not block local open.
- `test_click_notify_nonzero_does_not_xdg_open`: keep behavior unchanged. Acceptance: nonzero returncode still skips ack/open.
- `test_bridge_crash_isolation_logs_and_returns`: replace with crash-restart expectations. Acceptance: crash logs at error with `exc_info=True`, then supervisor restarts instead of returning dead.

Add tests:

- Dismissal stdout test. Acceptance: returncode `0` with empty stdout does not POST or `xdg-open`.
- Non-action stdout test. Acceptance: garbage or partial stdout does not POST or `xdg-open`.
- Finite read-timeout worker test. Acceptance: real `_sse_worker`, patched `chat_bridge.requests.get`, finite timeout tuple, and `_transport_error` frame on read timeout.
- Read-timeout invariant test. Acceptance: `SSE_READ_TIMEOUT_SECONDS > HEARTBEAT_STALE_SECONDS`, with the read value structurally derived from the stale value.
- Read-timeout reconnect integration test. Acceptance: real `_sse_worker` under `run_chat_bridge` turns a read timeout into the existing reconnect sleep path.
- Supervisor backoff and healthy-reset test. Acceptance: rapid crashes climb `[1, 2, 4, 8, 16, 30]`, capped at 30; a run lasting at least `HEALTHY_RUN_SECONDS` resets the sequence.
- Resource accounting across restarts test. Acceptance: each crashed incarnation runs cleanup; only one opt-in task is alive, and old pending notification tasks are cancelled before restart.
- Stop-during-backoff test. Acceptance: setting `stop_event` during supervisor backoff exits without another restart.

## Test-Seam Sanity Check

No existing patch target needs to move:

- Existing tests patch `chat_bridge._sse_worker`; `_run_bridge_body` will still call that module-level symbol.
- Existing tests patch `chat_bridge._opt_in_poll_loop`; `_run_bridge_body` will still create that task through the same symbol.
- Existing tests patch `chat_bridge.asyncio.sleep`; both reconnect and supervisor backoff will continue to flow through `_sleep_reconnect`.
- Existing notification tests patch `asyncio.create_subprocess_exec`, `chat_bridge.requests.post`, and `chat_bridge.subprocess.Popen`; `_handle_one_notification` remains the tested entry point.
- Existing disabled-config and terminal-auth tests still call `run_chat_bridge`. Config gates remain in `run_chat_bridge`; terminal 401/403 still returns normally without restart.

Expected intentional test impact:

- Click tests currently encode the dismissal bug because `FakeProc` returns empty stdout with returncode `0`.
- Crash isolation currently encodes crash-then-return and must change to crash-then-restart.

## Logging

Keep the current logging altitude:

- Transport details and notification dismissal remain `DEBUG`.
- Reconnect/restart state transitions remain `INFO`.
- Heartbeat stale remains `WARNING`.
- Genuine bridge crashes remain `ERROR` with `exc_info=True`.
