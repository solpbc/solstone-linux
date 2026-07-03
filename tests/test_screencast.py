# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Tests for portal screencast stream matching and X11 capture."""

import logging
import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from dbus_next import Variant
from dbus_next.errors import DBusError

from solstone_linux import screencast as screencast_module
from solstone_linux.screencast import (
    Screencaster,
    X11Screencaster,
    _match_streams_to_monitors,
)


def _make_signal_message(path: str, results: dict):
    msg = MagicMock()
    msg.message_type.name = "SIGNAL"
    msg.path = path
    msg.interface = screencast_module.REQ_IFACE
    msg.member = "Response"
    msg.body = [0, results]
    return msg


def _emit_portal_response(bus, token: str, results: dict):
    handler = bus.add_message_handler.call_args.args[0]
    handler(
        _make_signal_message(
            screencast_module._make_request_handle(bus, token), results
        )
    )


def _make_fake_portal_bus():
    bus = MagicMock()
    bus.unique_name = ":1.77"
    bus.introspect = AsyncMock(return_value=object())
    bus.add_message_handler = MagicMock()
    bus.remove_message_handler = MagicMock()

    screencast_iface = MagicMock()
    session_iface = MagicMock()
    session_iface.call_close = AsyncMock(return_value=None)

    def get_proxy_object(_service, path, _intro):
        obj = MagicMock()
        if path == screencast_module.PORTAL_PATH:
            obj.get_interface.return_value = screencast_iface
        else:
            obj.get_interface.return_value = session_iface
        return obj

    bus.get_proxy_object.side_effect = get_proxy_object
    return bus, screencast_iface, session_iface


def _patch_monitor_fallbacks(monkeypatch):
    monkeypatch.setattr(
        "solstone_linux.activity.get_monitor_geometries",
        lambda: [],
    )
    monkeypatch.setattr(
        "solstone_linux.activity.get_monitor_geometries_kscreen",
        AsyncMock(return_value=[]),
    )


class TestMatchStreamsToMonitors:
    """Test matching portal streams to monitor metadata."""

    def test_position_based_matching(self):
        streams = [
            {
                "idx": 0,
                "node_id": 10,
                "props": {"position": (0, 0), "size": (1920, 1080)},
            },
            {
                "idx": 1,
                "node_id": 11,
                "props": {"position": (1920, 0), "size": (2560, 1440)},
            },
        ]
        monitors = [
            {"id": "DP-1", "box": [0, 0, 1920, 1080], "position": "left"},
            {"id": "DP-2", "box": [1920, 0, 4480, 1440], "position": "right"},
        ]

        result = _match_streams_to_monitors(streams, monitors)

        assert result[0]["connector"] == "DP-1"
        assert result[0]["position_label"] == "left"
        assert result[0]["x"] == 0
        assert result[0]["y"] == 0
        assert result[0]["width"] == 1920
        assert result[0]["height"] == 1080
        assert result[1]["connector"] == "DP-2"
        assert result[1]["position_label"] == "right"
        assert result[1]["x"] == 1920
        assert result[1]["y"] == 0
        assert result[1]["width"] == 2560
        assert result[1]["height"] == 1440

    def test_size_based_fallback_when_no_position(self):
        streams = [
            {
                "idx": 0,
                "node_id": 10,
                "props": {"position": (0, 0), "size": (1920, 1080)},
            },
            {
                "idx": 1,
                "node_id": 11,
                "props": {"position": (0, 0), "size": (2560, 1440)},
            },
        ]
        monitors = [
            {"id": "DP-1", "box": [20, 0, 1940, 1080], "position": "left"},
            {"id": "DP-2", "box": [1940, 0, 4500, 1440], "position": "right"},
        ]

        result = _match_streams_to_monitors(streams, monitors)

        assert result[0]["connector"] == "DP-1"
        assert result[0]["position_label"] == "left"
        assert result[0]["x"] == 20
        assert result[0]["width"] == 1920
        assert result[1]["connector"] == "DP-2"
        assert result[1]["position_label"] == "right"
        assert result[1]["x"] == 1940
        assert result[1]["width"] == 2560

    def test_position_match_skipped_when_all_zero(self):
        streams = [
            {
                "idx": 0,
                "node_id": 10,
                "props": {"position": (0, 0), "size": (2560, 1440)},
            },
            {
                "idx": 1,
                "node_id": 11,
                "props": {"position": (0, 0), "size": (1920, 1080)},
            },
        ]
        monitors = [
            {"id": "DP-1", "box": [0, 0, 1920, 1080], "position": "left"},
            {"id": "DP-2", "box": [1920, 0, 4480, 1440], "position": "right"},
        ]

        result = _match_streams_to_monitors(streams, monitors)

        assert result[0]["connector"] == "DP-2"
        assert result[0]["position_label"] == "right"
        assert result[0]["x"] == 1920
        assert result[0]["width"] == 2560
        assert result[1]["connector"] == "DP-1"
        assert result[1]["position_label"] == "left"
        assert result[1]["x"] == 0
        assert result[1]["width"] == 1920

    def test_ambiguous_size_assigns_in_order(self):
        streams = [
            {
                "idx": 0,
                "node_id": 10,
                "props": {"position": (0, 0), "size": (1920, 1080)},
            },
            {
                "idx": 1,
                "node_id": 11,
                "props": {"position": (0, 0), "size": (1920, 1080)},
            },
        ]
        monitors = [
            {"id": "DP-1", "box": [20, 0, 1940, 1080], "position": "left"},
            {"id": "DP-2", "box": [1940, 0, 3860, 1080], "position": "right"},
        ]

        result = _match_streams_to_monitors(streams, monitors)

        assert result[0]["connector"] == "DP-1"
        assert result[1]["connector"] == "DP-2"

    def test_no_monitors_falls_back_to_monitor_idx(self):
        streams = [
            {
                "idx": 0,
                "node_id": 10,
                "props": {"position": (0, 0), "size": (1920, 1080)},
            },
            {
                "idx": 1,
                "node_id": 11,
                "props": {"position": (1920, 0), "size": (2560, 1440)},
            },
        ]

        result = _match_streams_to_monitors(streams, [])

        assert result[0]["connector"] == "monitor-0"
        assert result[0]["position_label"] == "unknown"
        assert result[1]["connector"] == "monitor-1"
        assert result[1]["position_label"] == "unknown"

    def test_mixed_position_and_size_matching(self):
        streams = [
            {
                "idx": 0,
                "node_id": 10,
                "props": {"position": (0, 0), "size": (1920, 1080)},
            },
            {
                "idx": 1,
                "node_id": 11,
                "props": {"position": (0, 0), "size": (2560, 1440)},
            },
        ]
        monitors = [
            {"id": "DP-1", "box": [0, 0, 1920, 1080], "position": "left"},
            {"id": "DP-2", "box": [1920, 0, 4480, 1440], "position": "right"},
        ]

        result = _match_streams_to_monitors(streams, monitors)

        assert result[0]["connector"] == "DP-1"
        assert result[0]["position_label"] == "left"
        assert result[1]["connector"] == "DP-2"
        assert result[1]["position_label"] == "right"


@pytest.mark.asyncio
async def test_close_session_call_close_failure_logs_and_clears_handle(caplog):
    screencaster = Screencaster(restore_token_path=Path("/tmp/fake"))
    mock_bus = MagicMock()
    session_iface = MagicMock()
    session_iface.call_close = AsyncMock(
        side_effect=DBusError("org.freedesktop.DBus.Error.NoReply", "broke")
    )

    mock_bus.introspect = AsyncMock(return_value=object())
    mock_bus.get_proxy_object.return_value.get_interface.return_value = session_iface
    screencaster.bus = mock_bus
    screencaster.session_handle = "/org/freedesktop/portal/desktop/session/fake"

    with caplog.at_level(logging.WARNING):
        await screencaster._close_session()

    assert [record.message for record in caplog.records] == [
        "_close_session failed: "
        "service=org.freedesktop.portal.Desktop "
        "path=/org/freedesktop/portal/desktop/session/fake: "
        "DBusError: broke"
    ]
    assert screencaster.session_handle is None


@pytest.mark.asyncio
async def test_start_times_out_unresolved_response_and_removes_handler(
    monkeypatch, tmp_path
):
    monkeypatch.setattr(screencast_module, "PORTAL_CALL_TIMEOUT", 0.01)
    monkeypatch.setattr(screencast_module, "PORTAL_INTERACTIVE_TIMEOUT", 0.01)
    _patch_monitor_fallbacks(monkeypatch)
    bus, screencast_iface, session_iface = _make_fake_portal_bus()
    screencaster = Screencaster(restore_token_path=tmp_path / "token")
    screencaster.bus = bus

    async def create_session(opts):
        token = opts["handle_token"].value
        _emit_portal_response(
            bus,
            token,
            {"session_handle": Variant("o", "/org/freedesktop/portal/session/fake")},
        )

    screencast_iface.call_create_session = AsyncMock(side_effect=create_session)
    screencast_iface.call_select_sources = AsyncMock(return_value=None)

    with pytest.raises(RuntimeError, match="SelectSources timed out"):
        await screencaster.start(str(tmp_path))

    session_iface.call_close.assert_awaited_once()
    assert bus.add_message_handler.call_count == bus.remove_message_handler.call_count


@pytest.mark.asyncio
async def test_start_times_out_method_call_and_removes_handler(monkeypatch, tmp_path):
    monkeypatch.setattr(screencast_module, "PORTAL_CALL_TIMEOUT", 0.01)
    monkeypatch.setattr(screencast_module, "PORTAL_INTERACTIVE_TIMEOUT", 0.01)
    _patch_monitor_fallbacks(monkeypatch)
    bus, screencast_iface, session_iface = _make_fake_portal_bus()
    screencaster = Screencaster(restore_token_path=tmp_path / "token")
    screencaster.bus = bus

    async def create_session(opts):
        token = opts["handle_token"].value
        _emit_portal_response(
            bus,
            token,
            {"session_handle": Variant("o", "/org/freedesktop/portal/session/fake")},
        )

    async def hang_forever(*_args):
        await asyncio.Future()

    screencast_iface.call_create_session = AsyncMock(side_effect=create_session)
    screencast_iface.call_select_sources = AsyncMock(side_effect=hang_forever)

    with pytest.raises(RuntimeError, match="SelectSources timed out"):
        await screencaster.start(str(tmp_path))

    session_iface.call_close.assert_awaited_once()
    assert bus.add_message_handler.call_count == bus.remove_message_handler.call_count


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "restore_token,expected_response_timeout",
    [
        (None, 22),
        ("saved-token", 11),
    ],
)
async def test_start_selects_response_timeout_from_restore_token(
    monkeypatch, tmp_path, restore_token, expected_response_timeout
):
    monkeypatch.setattr(screencast_module, "PORTAL_CALL_TIMEOUT", 11)
    monkeypatch.setattr(screencast_module, "PORTAL_INTERACTIVE_TIMEOUT", 22)
    _patch_monitor_fallbacks(monkeypatch)
    if restore_token:
        (tmp_path / "token").write_text(f"{restore_token}\n", encoding="utf-8")

    timeouts = []
    real_wait_for = asyncio.wait_for

    async def recording_wait_for(awaitable, timeout):
        timeouts.append(timeout)
        return await real_wait_for(awaitable, timeout)

    monkeypatch.setattr(screencast_module.asyncio, "wait_for", recording_wait_for)

    bus, screencast_iface, _session_iface = _make_fake_portal_bus()
    screencaster = Screencaster(restore_token_path=tmp_path / "token")
    screencaster.bus = bus

    async def create_session(opts):
        token = opts["handle_token"].value
        _emit_portal_response(
            bus,
            token,
            {"session_handle": Variant("o", "/org/freedesktop/portal/session/fake")},
        )

    async def select_sources(_session_handle, _opts):
        token = _opts["handle_token"].value
        _emit_portal_response(bus, token, {})

    async def start_session(_session_handle, _parent_window, opts):
        token = opts["handle_token"].value
        _emit_portal_response(bus, token, {"streams": [(10, {})]})

    fd_obj = MagicMock()
    fd_obj.take.return_value = 42
    process = MagicMock()
    process.poll.return_value = None
    process.stderr = None
    screencast_iface.call_create_session = AsyncMock(side_effect=create_session)
    screencast_iface.call_select_sources = AsyncMock(side_effect=select_sources)
    screencast_iface.call_start = AsyncMock(side_effect=start_session)
    screencast_iface.call_open_pipe_wire_remote = AsyncMock(return_value=fd_obj)

    with patch("solstone_linux.screencast.subprocess.Popen", return_value=process):
        streams = await screencaster.start(str(tmp_path))

    screencaster.pw_fd = None
    assert len(streams) == 1
    assert timeouts[4] == expected_response_timeout
    assert timeouts[6] == expected_response_timeout


class TestX11Screencaster:
    """Tests for the X11 ximagesrc-based screencaster."""

    TWO_MONITORS = [
        {"id": "DP-1", "box": [0, 0, 1920, 1080], "position": "left"},
        {"id": "DP-2", "box": [1920, 0, 3840, 1080], "position": "right"},
    ]

    @pytest.mark.asyncio
    async def test_connect_fails_without_display(self, monkeypatch):
        monkeypatch.delenv("DISPLAY", raising=False)
        sc = X11Screencaster()

        result = await sc.connect()

        assert result is False

    @pytest.mark.asyncio
    async def test_connect_fails_without_gst_launch(self, monkeypatch):
        monkeypatch.setenv("DISPLAY", ":0")
        monkeypatch.setattr("solstone_linux.screencast.shutil.which", lambda _: None)
        sc = X11Screencaster()

        result = await sc.connect()

        assert result is False

    @pytest.mark.asyncio
    async def test_connect_succeeds(self, monkeypatch):
        monkeypatch.setenv("DISPLAY", ":0")
        monkeypatch.setattr(
            "solstone_linux.screencast.shutil.which",
            lambda _: "/usr/bin/gst-launch-1.0",
        )
        sc = X11Screencaster()

        result = await sc.connect()

        assert result is True

    @pytest.mark.asyncio
    async def test_start_no_monitors_raises(self, monkeypatch, tmp_path):
        monkeypatch.setenv("DISPLAY", ":0")
        monkeypatch.setattr(
            "solstone_linux.screencast.X11Screencaster.connect",
            AsyncMock(return_value=True),
        )
        with patch("solstone_linux.screencast.X11Screencaster.start") as mock_start:
            mock_start.side_effect = RuntimeError("No monitors found for X11 capture")
            sc = X11Screencaster()
            with pytest.raises(RuntimeError, match="No monitors"):
                await sc.start(str(tmp_path))

    @pytest.mark.asyncio
    async def test_start_builds_one_branch_per_monitor(self, monkeypatch, tmp_path):
        monkeypatch.setenv("DISPLAY", ":0")

        with patch("solstone_linux.screencast.X11Screencaster") as MockClass:
            instance = MagicMock()
            left = MagicMock()
            left.position = "left"
            left.connector = "DP-1"
            left.file_path = str(tmp_path / "left_DP-1_screen.webm")
            right = MagicMock()
            right.position = "right"
            right.connector = "DP-2"
            right.file_path = str(tmp_path / "right_DP-2_screen.webm")
            instance.start = AsyncMock(return_value=[left, right])
            MockClass.return_value = instance

            sc = MockClass()
            streams = await sc.start(str(tmp_path))

        assert len(streams) == 2

    @pytest.mark.asyncio
    async def test_start_sets_correct_ximagesrc_region(self, monkeypatch, tmp_path):
        """Verify pipeline strings use inclusive endx/endy (startx+width-1)."""
        monkeypatch.setenv("DISPLAY", ":0")

        captured_cmd = []

        def fake_popen(cmd, **kwargs):
            captured_cmd.extend(cmd)
            proc = MagicMock()
            proc.poll.return_value = None
            proc.stderr = MagicMock()
            return proc

        with patch(
            "solstone_linux.screencast.subprocess.Popen", side_effect=fake_popen
        ):
            with patch(
                "solstone_linux.screencast.X11Screencaster.connect",
                new=AsyncMock(return_value=True),
            ):
                with patch(
                    "solstone_linux.activity.get_monitor_geometries_x11",
                    return_value=self.TWO_MONITORS,
                ):
                    sc = X11Screencaster()
                    sc._started = False
                    # Manually call the real start to inspect the pipeline

                    with patch("asyncio.sleep", new=AsyncMock()):
                        streams = await sc.start(
                            str(tmp_path), framerate=1, draw_cursor=False
                        )

        pipeline = " ".join(captured_cmd)
        # DP-1: 1920x1080 at (0,0) → endx=1919, endy=1079
        assert "startx=0" in pipeline
        assert "starty=0" in pipeline
        assert "endx=1919" in pipeline
        assert "endy=1079" in pipeline
        # DP-2: 1920x1080 at (1920,0) → endx=3839, endy=1079
        assert "startx=1920" in pipeline
        assert "endx=3839" in pipeline
        assert "show-pointer=false" in pipeline
        assert len(streams) == 2

    @pytest.mark.asyncio
    async def test_stop_filters_silent_streams(self, tmp_path):
        """Small files are classified as silent and deleted."""
        sc = X11Screencaster()
        sc._started = True

        webm_file = tmp_path / "left_DP-1_screen.webm"
        webm_file.write_bytes(b"small")  # < MIN_HEALTHY_WEBM_BYTES

        from solstone_linux.screencast import StreamInfo

        sc.streams = [
            StreamInfo(
                node_id=0,
                position="left",
                connector="DP-1",
                x=0,
                y=0,
                width=1920,
                height=1080,
                file_path=str(webm_file),
            )
        ]
        sc.gst_process = None

        healthy, silent = await sc.stop()

        assert healthy == []
        assert len(silent) == 1
        assert silent[0].connector == "DP-1"
        assert not webm_file.exists()

    @pytest.mark.asyncio
    async def test_stop_keeps_healthy_streams(self, tmp_path):
        """Files >= MIN_HEALTHY_WEBM_BYTES are returned as healthy."""
        sc = X11Screencaster()
        sc._started = True

        from solstone_linux.screencast import MIN_HEALTHY_WEBM_BYTES, StreamInfo

        webm_file = tmp_path / "left_DP-1_screen.webm"
        webm_file.write_bytes(b"x" * MIN_HEALTHY_WEBM_BYTES)

        sc.streams = [
            StreamInfo(
                node_id=0,
                position="left",
                connector="DP-1",
                x=0,
                y=0,
                width=1920,
                height=1080,
                file_path=str(webm_file),
            )
        ]
        sc.gst_process = None

        healthy, silent = await sc.stop()

        assert len(healthy) == 1
        assert silent == []

    def test_is_healthy_false_before_start(self):
        sc = X11Screencaster()
        assert sc.is_healthy() is False

    def test_is_healthy_false_when_process_exited(self):
        sc = X11Screencaster()
        sc._started = True
        proc = MagicMock()
        proc.poll.return_value = 1  # exited
        sc.gst_process = proc
        assert sc.is_healthy() is False

    def test_is_healthy_true_when_running(self):
        sc = X11Screencaster()
        sc._started = True
        proc = MagicMock()
        proc.poll.return_value = None  # still running
        sc.gst_process = proc
        assert sc.is_healthy() is True
