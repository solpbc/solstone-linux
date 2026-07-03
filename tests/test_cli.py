# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import argparse
import os
import sys
from pathlib import Path
from unittest.mock import MagicMock
from unittest.mock import patch

import pytest

from solstone_linux import __version__
from solstone_linux import cli as cli_module
from solstone_linux.cli import (
    _cmd_setup_interactive,
    cmd_install_service,
    cmd_setup,
    cmd_settings,
    cmd_status,
)
from solstone_linux.config import (
    Config,
    DEFAULT_SERVER_URL,
    load_config as real_load_config,
)
from solstone_linux.sync_health import ErrorType, SyncFacts, save_facts


def _args() -> argparse.Namespace:
    return argparse.Namespace()


def _settings_config(tmp_path: Path) -> Config:
    return Config(
        base_dir=tmp_path,
        config_dir=tmp_path / "config",
        server_url="https://id",
        key="KKKK",
        stream="strm",
        capture_framerate=2,
    )


def _run_settings(tmp_path: Path, inputs: list[str]) -> Config:
    config = _settings_config(tmp_path)

    with patch("solstone_linux.cli.load_config", return_value=config):
        with patch("solstone_linux.cli.save_config") as save_mock:
            with patch("builtins.input", side_effect=inputs):
                assert cmd_settings(_args()) == 0

    return save_mock.call_args.args[0]


def test_main_version_flag(monkeypatch, capsys):
    monkeypatch.setattr(sys, "argv", ["solstone-linux", "--version"])

    with pytest.raises(SystemExit) as excinfo:
        cli_module.main()

    assert excinfo.value.code == 0
    out = capsys.readouterr().out
    assert __version__ in out


_BINARY = "/home/user/.local/pipx/venvs/solstone-linux/bin/solstone-linux"
_EXPECTED_SVGS = {
    "solstone-error.svg",
    "solstone-paused.svg",
    "solstone-recording.svg",
    "solstone-syncing.svg",
}


_REAL_IS_DIR = Path.is_dir


def _is_dir_without_icons(self: Path) -> bool:
    icon_source = Path(cli_module.__file__).resolve().parent / "icons" / "hicolor"
    if self == icon_source:
        return False
    return _REAL_IS_DIR(self)


def test_cmd_settings_enter_keeps_all(tmp_path: Path):
    saved_config = _run_settings(tmp_path, ["", "", "", "", "", ""])

    assert saved_config.capture_framerate == 2
    assert saved_config.draw_cursor is True
    assert saved_config.start_paused is False
    assert saved_config.segment_interval == 300
    assert saved_config.chat_bridge_enabled is True
    assert saved_config.cache_retention_days == 7
    assert saved_config.server_url == "https://id"
    assert saved_config.key == "KKKK"
    assert saved_config.stream == "strm"


def test_cmd_settings_changes_framerate(tmp_path: Path):
    saved_config = _run_settings(tmp_path, ["5", "", "", "", "", ""])

    assert saved_config.capture_framerate == 5
    assert saved_config.server_url == "https://id"
    assert saved_config.key == "KKKK"
    assert saved_config.stream == "strm"


def test_cmd_settings_framerate_clamped(tmp_path: Path):
    saved_config = _run_settings(tmp_path, ["99", "", "", "", "", ""])

    assert saved_config.capture_framerate == 10


def test_cmd_settings_framerate_reprompts_on_invalid(tmp_path: Path):
    saved_config = _run_settings(tmp_path, ["abc", "3", "", "", "", "", ""])

    assert saved_config.capture_framerate == 3


def test_cmd_settings_toggles_bool(tmp_path: Path):
    saved_config = _run_settings(tmp_path, ["", "n", "", "", "", ""])

    assert saved_config.draw_cursor is False
    assert saved_config.server_url == "https://id"
    assert saved_config.key == "KKKK"
    assert saved_config.stream == "strm"


def test_cmd_settings_retention_semantics(tmp_path: Path):
    saved_config = _run_settings(tmp_path, ["", "", "", "", "", "-1"])

    assert saved_config.cache_retention_days == -1


def test_cmd_status_prints_sync_health(tmp_path: Path, monkeypatch, capsys):
    config = Config(
        base_dir=tmp_path,
        server_url="https://test.example.com",
        key="K123456789",
        stream="test-stream",
    )
    config.ensure_dirs()
    save_facts(config.state_dir, SyncFacts(last_error_class=ErrorType.TRANSIENT))
    monkeypatch.setattr(cli_module, "load_config", lambda: config)
    monkeypatch.setattr(
        cli_module.subprocess,
        "run",
        MagicMock(return_value=MagicMock(stdout="active\n")),
    )

    assert cmd_status(_args()) == 0

    out = capsys.readouterr().out
    assert "Sync: offline — saving locally; pending unconfirmed (will retry)" in out
    assert "Synced:" not in out


def test_cmd_status_handles_corrupt_config(tmp_path: Path, monkeypatch):
    config_dir = tmp_path / "config"
    config_dir.mkdir(parents=True)
    (config_dir / "config.json").write_text("[]")

    monkeypatch.setattr(
        cli_module,
        "load_config",
        lambda: real_load_config(base_dir=tmp_path, config_dir=config_dir),
    )
    monkeypatch.setattr(
        cli_module.subprocess,
        "run",
        MagicMock(return_value=MagicMock(stdout="inactive\n")),
    )

    assert cmd_status(_args()) == 0


def test_cmd_install_service_uses_environment_path(tmp_path: Path):
    binary = "/home/user/.local/pipx/venvs/solstone-linux/bin/solstone-linux"
    unit_path = tmp_path / ".config" / "systemd" / "user" / "solstone-linux.service"
    env = {
        "PATH": "/home/user/.local/pipx/venvs/solstone-linux/bin:/usr/local/bin:/usr/bin:/bin:/home/user/.local/bin"
    }

    with patch.dict(os.environ, env, clear=True):
        with patch("solstone_linux.cli.shutil.which", return_value=binary):
            with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
                with patch("solstone_linux.cli.subprocess.run"):
                    with patch("solstone_linux.cli.Path.is_dir", return_value=False):
                        assert cmd_install_service(_args()) == 0

    unit_content = unit_path.read_text()
    assert "PartOf=graphical-session.target" in unit_content
    assert "WantedBy=graphical-session.target" in unit_content
    path_line = next(
        line
        for line in unit_content.splitlines()
        if line.startswith("Environment=PATH=")
    )
    service_path = path_line.removeprefix("Environment=PATH=").split(":")

    assert service_path[0] == "/home/user/.local/pipx/venvs/solstone-linux/bin"
    assert service_path == list(dict.fromkeys(service_path))


def test_cmd_install_service_uses_default_path_when_missing(tmp_path: Path):
    binary = "/home/user/.local/pipx/venvs/solstone-linux/bin/solstone-linux"
    unit_path = tmp_path / ".config" / "systemd" / "user" / "solstone-linux.service"

    with patch.dict(os.environ, {}, clear=True):
        with patch("solstone_linux.cli.shutil.which", return_value=binary):
            with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
                with patch("solstone_linux.cli.subprocess.run"):
                    with patch("solstone_linux.cli.Path.is_dir", return_value=False):
                        assert cmd_install_service(_args()) == 0

    unit_content = unit_path.read_text()
    path_line = next(
        line
        for line in unit_content.splitlines()
        if line.startswith("Environment=PATH=")
    )

    assert (
        path_line
        == "Environment=PATH=/home/user/.local/pipx/venvs/solstone-linux/bin:/usr/local/bin:/usr/bin:/bin"
    )


def test_cmd_install_service_uses_default_path_when_empty(tmp_path: Path):
    binary = "/home/user/.local/pipx/venvs/solstone-linux/bin/solstone-linux"
    unit_path = tmp_path / ".config" / "systemd" / "user" / "solstone-linux.service"

    with patch.dict(os.environ, {"PATH": ""}, clear=True):
        with patch("solstone_linux.cli.shutil.which", return_value=binary):
            with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
                with patch("solstone_linux.cli.subprocess.run"):
                    with patch("solstone_linux.cli.Path.is_dir", return_value=False):
                        assert cmd_install_service(_args()) == 0

    unit_content = unit_path.read_text()
    path_line = next(
        line
        for line in unit_content.splitlines()
        if line.startswith("Environment=PATH=")
    )

    assert (
        path_line
        == "Environment=PATH=/home/user/.local/pipx/venvs/solstone-linux/bin:/usr/local/bin:/usr/bin:/bin"
    )


def test_cmd_install_service_always_rewrites(tmp_path: Path, capsys):
    binary = "/home/user/.local/pipx/venvs/solstone-linux/bin/solstone-linux"

    with patch.dict(os.environ, {"PATH": "/usr/local/bin:/usr/bin:/bin"}, clear=True):
        with patch("solstone_linux.cli.shutil.which", return_value=binary):
            with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
                with patch("solstone_linux.cli.subprocess.run") as run_mock:
                    with patch(
                        "solstone_linux.cli.Path.is_dir",
                        autospec=True,
                        side_effect=_is_dir_without_icons,
                    ):
                        assert cmd_install_service(_args()) == 0
                        assert cmd_install_service(_args()) == 0

    captured = capsys.readouterr()
    assert "nothing to do" not in captured.out.lower()
    assert run_mock.call_count == 8


def test_cmd_install_service_installs_svgs_without_index_theme(tmp_path: Path):
    with patch("solstone_linux.cli.shutil.which", return_value=_BINARY):
        with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
            with patch("solstone_linux.cli.subprocess.run"):
                assert cmd_install_service(_args()) == 0

    hicolor = tmp_path / ".local/share/icons/hicolor"
    status = hicolor / "scalable/status"

    assert {path.name for path in status.glob("*.svg")} == _EXPECTED_SVGS
    assert not (hicolor / "index.theme").exists()


def test_cmd_install_service_removes_stale_solstone_index_theme(tmp_path: Path):
    hicolor = tmp_path / ".local/share/icons/hicolor"
    hicolor.mkdir(parents=True)
    (hicolor / "index.theme").write_text(
        "[Icon Theme]\nName=solstone\nInherits=hicolor\nDirectories=scalable/status\n"
    )

    with patch("solstone_linux.cli.shutil.which", return_value=_BINARY):
        with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
            with patch("solstone_linux.cli.subprocess.run"):
                assert cmd_install_service(_args()) == 0

    assert not (hicolor / "index.theme").exists()


def test_cmd_install_service_keeps_foreign_index_theme(tmp_path: Path):
    hicolor = tmp_path / ".local/share/icons/hicolor"
    hicolor.mkdir(parents=True)
    index = hicolor / "index.theme"
    content = "[Icon Theme]\nName=MyTheme\nName=solstone-custom\n"
    index.write_text(content)

    with patch("solstone_linux.cli.shutil.which", return_value=_BINARY):
        with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
            with patch("solstone_linux.cli.subprocess.run"):
                assert cmd_install_service(_args()) == 0

    assert index.exists()
    assert index.read_text() == content


def test_cmd_install_service_reports_unreadable_index_theme(
    tmp_path: Path,
    capsys,
):
    hicolor = tmp_path / ".local/share/icons/hicolor"
    hicolor.mkdir(parents=True)
    index = hicolor / "index.theme"
    index.write_bytes(b"\xff\xfe\x00not utf8")

    with patch("solstone_linux.cli.shutil.which", return_value=_BINARY):
        with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
            with patch("solstone_linux.cli.subprocess.run"):
                assert cmd_install_service(_args()) == 0

    captured = capsys.readouterr()
    warning_lines = [
        line
        for line in captured.out.splitlines()
        if "Left existing icon theme index in place" in line
    ]

    assert index.exists()
    assert len(warning_lines) == 1
    assert str(index) in warning_lines[0]


def test_cmd_install_service_icon_step_idempotent(tmp_path: Path):
    with patch("solstone_linux.cli.shutil.which", return_value=_BINARY):
        with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
            with patch("solstone_linux.cli.subprocess.run"):
                assert cmd_install_service(_args()) == 0
                assert cmd_install_service(_args()) == 0

    hicolor = tmp_path / ".local/share/icons/hicolor"
    status = hicolor / "scalable/status"

    assert {path.name for path in status.glob("*.svg")} == _EXPECTED_SVGS
    assert not (hicolor / "index.theme").exists()


def test_cmd_install_service_survives_missing_gtk_update_icon_cache(tmp_path: Path):
    with patch("solstone_linux.cli.shutil.which", return_value=_BINARY):
        with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
            with patch(
                "solstone_linux.cli.subprocess.run",
                side_effect=FileNotFoundError,
            ):
                assert cmd_install_service(_args()) == 0

    hicolor = tmp_path / ".local/share/icons/hicolor"
    status = hicolor / "scalable/status"

    assert {path.name for path in status.glob("*.svg")} == _EXPECTED_SVGS
    assert not (hicolor / "index.theme").exists()


def test_cmd_install_service_survives_nonzero_gtk_update_icon_cache(tmp_path: Path):
    with patch("solstone_linux.cli.shutil.which", return_value=_BINARY):
        with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
            with patch("solstone_linux.cli.subprocess.run") as run_mock:
                run_mock.return_value = MagicMock(returncode=1)

                assert cmd_install_service(_args()) == 0


def test_cmd_install_service_writes_autostart_entry(tmp_path: Path):
    with patch("solstone_linux.cli.shutil.which", return_value=_BINARY):
        with patch("solstone_linux.cli.Path.home", return_value=tmp_path):
            with patch("solstone_linux.cli.subprocess.run"):
                assert cmd_install_service(_args()) == 0

    autostart = tmp_path / ".config" / "autostart" / "solstone-linux.desktop"
    assert autostart.exists()
    content = autostart.read_text()
    assert "Type=Application" in content
    assert "solstone-linux.service" in content
    assert "import-environment" in content
    assert "DISPLAY" in content
    assert "XAUTHORITY" in content
    assert "XDG_SESSION_TYPE" in content


def test_cmd_setup_non_interactive_happy_path(tmp_path: Path):
    args = argparse.Namespace(
        server_url="https://x",
        token="t",
        stream_name=None,
        non_interactive=True,
    )
    config = Config(base_dir=tmp_path)

    with patch("solstone_linux.cli.load_config", return_value=config):
        with patch("solstone_linux.cli.save_config") as save_mock:
            with patch("solstone_linux.cli.streams.stream_name", return_value="host-a"):
                with patch("solstone_linux.upload.UploadClient.ensure_registered"):
                    assert cmd_setup(args) == 0

    saved_config = save_mock.call_args.args[0]
    assert saved_config.server_url == "https://x"
    assert saved_config.key == "t"
    assert saved_config.stream == "host-a"


def test_cmd_setup_non_interactive_defaults_server_url(tmp_path: Path, capsys):
    args = argparse.Namespace(
        server_url=None,
        token=None,
        stream_name=None,
        non_interactive=True,
    )
    config = Config(base_dir=tmp_path)

    with patch.dict(os.environ, {}, clear=True):
        with patch("solstone_linux.cli.load_config", return_value=config):
            with patch("solstone_linux.cli.save_config") as save_mock:
                with patch(
                    "solstone_linux.cli.streams.stream_name", return_value="host-a"
                ):
                    with patch(
                        "solstone_linux.upload.UploadClient.ensure_registered",
                        return_value=True,
                    ):
                        assert cmd_setup(args) == 0

    saved_config = save_mock.call_args.args[0]
    captured = capsys.readouterr()
    assert saved_config.server_url == DEFAULT_SERVER_URL
    assert "--server-url" not in captured.err
    assert "required" not in captured.err


def test_cmd_setup_server_url_override_persists(tmp_path: Path):
    args = argparse.Namespace(
        server_url="http://192.168.1.50:5015",
        token=None,
        stream_name=None,
        non_interactive=True,
    )
    config = Config(base_dir=tmp_path)

    with patch.dict(os.environ, {}, clear=True):
        with patch("solstone_linux.cli.load_config", return_value=config):
            with patch("solstone_linux.cli.save_config") as save_mock:
                with patch(
                    "solstone_linux.cli.streams.stream_name", return_value="host-a"
                ):
                    with patch(
                        "solstone_linux.upload.UploadClient.ensure_registered",
                        return_value=True,
                    ):
                        assert cmd_setup(args) == 0

    saved_config = save_mock.call_args.args[0]
    assert saved_config.server_url == "http://192.168.1.50:5015"


def test_cmd_setup_preserves_existing_server_url(tmp_path: Path):
    args = argparse.Namespace(
        server_url=None,
        token=None,
        stream_name=None,
        non_interactive=True,
    )
    config = Config(base_dir=tmp_path)
    config.server_url = "https://saved.example"

    with patch.dict(os.environ, {}, clear=True):
        with patch("solstone_linux.cli.load_config", return_value=config):
            with patch("solstone_linux.cli.save_config") as save_mock:
                with patch(
                    "solstone_linux.cli.streams.stream_name", return_value="host-a"
                ):
                    with patch(
                        "solstone_linux.upload.UploadClient.ensure_registered",
                        return_value=True,
                    ):
                        assert cmd_setup(args) == 0

    saved_config = save_mock.call_args.args[0]
    assert saved_config.server_url == "https://saved.example"


def test_cmd_setup_flagged_interactive_empty_input_defaults(tmp_path: Path):
    args = argparse.Namespace(
        server_url=None,
        token=None,
        stream_name="host-x",
        non_interactive=False,
    )
    config = Config(base_dir=tmp_path)

    with patch.dict(os.environ, {}, clear=True):
        with patch("solstone_linux.cli.load_config", return_value=config):
            with patch("solstone_linux.cli.save_config") as save_mock:
                with patch(
                    "solstone_linux.cli.streams.stream_name", return_value="host-a"
                ):
                    with patch("builtins.input", return_value=""):
                        with patch(
                            "solstone_linux.upload.UploadClient.ensure_registered",
                            return_value=True,
                        ):
                            assert cmd_setup(args) == 0

    saved_config = save_mock.call_args.args[0]
    assert saved_config.server_url == DEFAULT_SERVER_URL


def test_cmd_setup_interactive_legacy_empty_input_defaults(tmp_path: Path):
    config = Config(base_dir=tmp_path)

    with patch("solstone_linux.cli.load_config", return_value=config):
        with patch("solstone_linux.cli.save_config") as save_mock:
            with patch("solstone_linux.cli.stream_name", return_value="host-a"):
                with patch("builtins.input", return_value=""):
                    with patch(
                        "solstone_linux.upload.UploadClient.ensure_registered",
                        return_value=True,
                    ):
                        assert _cmd_setup_interactive() == 0

    saved_config = save_mock.call_args.args[0]
    assert saved_config.server_url == DEFAULT_SERVER_URL


def test_cmd_setup_env_token_fallback(tmp_path: Path, capsys):
    args = argparse.Namespace(
        server_url="https://x",
        token=None,
        stream_name=None,
        non_interactive=True,
    )
    config = Config(base_dir=tmp_path)

    with patch.dict(os.environ, {"SOLSTONE_TOKEN": "envtok"}, clear=True):
        with patch("solstone_linux.cli.load_config", return_value=config):
            with patch("solstone_linux.cli.save_config") as save_mock:
                with patch(
                    "solstone_linux.cli.streams.stream_name",
                    return_value="host-a",
                ):
                    with patch("solstone_linux.upload.UploadClient.ensure_registered"):
                        assert cmd_setup(args) == 0

    saved_config = save_mock.call_args.args[0]
    captured = capsys.readouterr()
    assert saved_config.key == "envtok"
    assert "shared computers" not in captured.err


def test_cmd_setup_cli_token_beats_env(tmp_path: Path, capsys):
    args = argparse.Namespace(
        server_url="https://x",
        token="clitok",
        stream_name=None,
        non_interactive=True,
    )
    config = Config(base_dir=tmp_path)

    with patch.dict(os.environ, {"SOLSTONE_TOKEN": "envtok"}, clear=True):
        with patch("solstone_linux.cli.load_config", return_value=config):
            with patch("solstone_linux.cli.save_config") as save_mock:
                with patch(
                    "solstone_linux.cli.streams.stream_name",
                    return_value="host-a",
                ):
                    with patch("solstone_linux.upload.UploadClient.ensure_registered"):
                        assert cmd_setup(args) == 0

    saved_config = save_mock.call_args.args[0]
    captured = capsys.readouterr()
    assert saved_config.key == "clitok"
    assert "shared computers" in captured.err


def test_cmd_setup_registers_via_http_when_no_token(tmp_path: Path):
    args = argparse.Namespace(
        server_url="http://localhost:9",
        token=None,
        stream_name=None,
        non_interactive=True,
    )
    config = Config(base_dir=tmp_path)

    def _register(cfg):
        cfg.key = "newkey00"
        cfg.stream = "locked-stream"
        return True

    with patch.dict(os.environ, {}, clear=True):
        with patch("solstone_linux.cli.load_config", return_value=config):
            with patch("solstone_linux.cli.save_config"):
                with patch(
                    "solstone_linux.cli.streams.stream_name", return_value="host-a"
                ):
                    with patch(
                        "solstone_linux.upload.UploadClient.ensure_registered",
                        side_effect=_register,
                    ) as reg_mock:
                        assert cmd_setup(args) == 0

    reg_mock.assert_called_once()
    assert config.key == "newkey00"
    assert config.stream == "locked-stream"


def test_cmd_setup_http_register_failure_non_interactive_returns_1(
    tmp_path: Path, capsys
):
    args = argparse.Namespace(
        server_url="http://localhost:9",
        token=None,
        stream_name=None,
        non_interactive=True,
    )
    config = Config(base_dir=tmp_path)

    with patch.dict(os.environ, {}, clear=True):
        with patch("solstone_linux.cli.load_config", return_value=config):
            with patch("solstone_linux.cli.save_config"):
                with patch(
                    "solstone_linux.cli.streams.stream_name", return_value="host-a"
                ):
                    with patch(
                        "solstone_linux.upload.UploadClient.ensure_registered",
                        return_value=False,
                    ):
                        assert cmd_setup(args) == 1

    captured = capsys.readouterr()
    assert "registration failed" in captured.out.lower()
