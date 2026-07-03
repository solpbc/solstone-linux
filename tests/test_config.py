# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import json
import logging
import os
import stat
from pathlib import Path

import pytest

from solstone_linux.config import (
    DEFAULT_SEGMENT_INTERVAL,
    DEFAULT_SYNC_MAX_RETRIES,
    DEFAULT_SYNC_RETRY_DELAYS,
    Config,
    load_config,
    save_config,
)


class TestConfig:
    def test_defaults(self):
        config = Config()
        assert config.server_url == ""
        assert config.key == ""
        assert config.segment_interval == 300

    def test_captures_dir(self):
        config = Config()
        assert config.captures_dir == config.base_dir / "captures"

    def test_restore_token_path(self):
        config = Config()
        assert config.restore_token_path == config.config_dir / "restore_token"

    def test_config_dir_uses_absolute_xdg(self, tmp_path: Path, monkeypatch):
        monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
        config = Config()

        assert config.config_dir == tmp_path / "solstone-linux"
        assert config.config_path == tmp_path / "solstone-linux" / "config.json"
        assert (
            config.restore_token_path == tmp_path / "solstone-linux" / "restore_token"
        )

    def test_config_dir_ignores_relative_xdg(self, monkeypatch):
        monkeypatch.setenv("XDG_CONFIG_HOME", "relative/path")

        assert Config().config_dir == Path.home() / ".config" / "solstone-linux"

    def test_config_dir_falls_back_when_xdg_unset(self, monkeypatch):
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)

        assert Config().config_dir == Path.home() / ".config" / "solstone-linux"

    def test_round_trip(self, tmp_path: Path):
        config = Config(base_dir=tmp_path, config_dir=tmp_path / "config")
        config.server_url = "https://example.com"
        config.key = "test-key-123"
        config.stream = "archon"
        config.segment_interval = 600

        save_config(config)

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.server_url == "https://example.com"
        assert loaded.key == "test-key-123"
        assert loaded.stream == "archon"
        assert loaded.segment_interval == 600

    def test_load_missing(self, tmp_path: Path):
        config = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert config.server_url == ""
        assert config.key == ""

    def test_load_corrupt(self, tmp_path: Path):
        config_dir = tmp_path / "config"
        config_dir.mkdir(parents=True)
        (config_dir / "config.json").write_text("not json!")

        config = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert config.server_url == ""

    @pytest.mark.parametrize(
        ("field", "value", "expected"),
        [
            ("capture_framerate", "abc", 1),
            ("segment_interval", "300", DEFAULT_SEGMENT_INTERVAL),
            ("sync_retry_delays", "oops", list(DEFAULT_SYNC_RETRY_DELAYS)),
            ("sync_max_retries", "many", DEFAULT_SYNC_MAX_RETRIES),
            ("cache_retention_days", [], 7),
        ],
    )
    def test_load_invalid_typed_fields_warn_and_default(
        self,
        tmp_path: Path,
        caplog,
        field,
        value,
        expected,
    ):
        config_dir = tmp_path / "config"
        config_dir.mkdir(parents=True)
        (config_dir / "config.json").write_text(json.dumps({field: value}))

        with caplog.at_level(logging.WARNING):
            config = load_config(base_dir=tmp_path, config_dir=config_dir)

        assert getattr(config, field) == expected
        warning_records = [
            record for record in caplog.records if field in record.message
        ]
        assert len(warning_records) == 1

    def test_load_non_object_json_warns_and_defaults(self, tmp_path: Path, caplog):
        config_dir = tmp_path / "config"
        config_dir.mkdir(parents=True)
        (config_dir / "config.json").write_text("[]")

        with caplog.at_level(logging.WARNING):
            config = load_config(base_dir=tmp_path, config_dir=config_dir)

        assert config.server_url == ""
        assert config.key == ""
        assert config.stream == ""
        assert config.segment_interval == DEFAULT_SEGMENT_INTERVAL
        assert config.sync_retry_delays == list(DEFAULT_SYNC_RETRY_DELAYS)
        assert config.sync_max_retries == DEFAULT_SYNC_MAX_RETRIES
        assert config.cache_retention_days == 7
        assert config.capture_framerate == 1
        warning_records = [
            record for record in caplog.records if "not a JSON object" in record.message
        ]
        assert len(warning_records) == 1

    def test_permissions(self, tmp_path: Path):
        config = Config(base_dir=tmp_path, config_dir=tmp_path / "config")
        config.server_url = "https://example.com"
        config.key = "secret"
        save_config(config)

        mode = config.config_path.stat().st_mode & 0o777
        assert mode == 0o600

    def test_sync_config_roundtrip(self, tmp_path: Path):
        config = Config(base_dir=tmp_path, config_dir=tmp_path / "config")
        config.sync_retry_delays = [10, 60, 300]
        config.sync_max_retries = 5
        save_config(config)

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.sync_retry_delays == [10, 60, 300]
        assert loaded.sync_max_retries == 5

    def test_cache_retention_days_roundtrip(self, tmp_path: Path):
        config = Config(base_dir=tmp_path, config_dir=tmp_path / "config")
        config.cache_retention_days = 14
        save_config(config)

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.cache_retention_days == 14

    def test_cache_retention_days_default(self, tmp_path: Path):
        """Existing configs without cache_retention_days default to 7."""
        config_dir = tmp_path / "config"
        config_dir.mkdir(parents=True)
        (config_dir / "config.json").write_text('{"server_url": "http://test"}')

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.cache_retention_days == 7

    def test_capture_framerate_default(self):
        config = Config()
        assert config.capture_framerate == 1

    def test_draw_cursor_default(self):
        config = Config()
        assert config.draw_cursor is True

    def test_capture_framerate_roundtrip(self, tmp_path: Path):
        config = Config(base_dir=tmp_path, config_dir=tmp_path / "config")
        config.capture_framerate = 2
        save_config(config)

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.capture_framerate == 2

    def test_draw_cursor_roundtrip(self, tmp_path: Path):
        config = Config(base_dir=tmp_path, config_dir=tmp_path / "config")
        config.draw_cursor = False
        save_config(config)

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.draw_cursor is False

    def test_capture_framerate_defaults_on_old_config(self, tmp_path: Path):
        """Existing configs without capture_framerate default to 1."""
        config_dir = tmp_path / "config"
        config_dir.mkdir(parents=True)
        (config_dir / "config.json").write_text('{"server_url": "http://test"}')

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.capture_framerate == 1
        assert loaded.draw_cursor is True

    def test_capture_framerate_clamped_to_max(self, tmp_path: Path):
        config_dir = tmp_path / "config"
        config_dir.mkdir(parents=True)
        (config_dir / "config.json").write_text('{"capture_framerate": 999}')

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.capture_framerate == 10

    def test_capture_framerate_clamped_to_min(self, tmp_path: Path):
        config_dir = tmp_path / "config"
        config_dir.mkdir(parents=True)
        (config_dir / "config.json").write_text('{"capture_framerate": 0}')

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.capture_framerate == 1

    def test_start_paused_default(self):
        config = Config()
        assert config.start_paused is False

    def test_start_paused_roundtrip(self, tmp_path: Path):
        config = Config(base_dir=tmp_path, config_dir=tmp_path / "config")
        config.start_paused = True
        save_config(config)

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.start_paused is True

    def test_start_paused_defaults_on_old_config(self, tmp_path: Path):
        """Existing configs without start_paused default to False."""
        config_dir = tmp_path / "config"
        config_dir.mkdir(parents=True)
        (config_dir / "config.json").write_text('{"server_url": "http://test"}')

        loaded = load_config(base_dir=tmp_path, config_dir=tmp_path / "config")
        assert loaded.start_paused is False

    def test_migrates_legacy_config(self, tmp_path: Path, caplog):
        old_dir = tmp_path / "config"
        old_dir.mkdir()
        old_config = old_dir / "config.json"
        old_config.write_text(
            json.dumps(
                {
                    "server_url": "https://example.com",
                    "key": "test-key-123",
                    "stream": "archon",
                    "capture_framerate": 3,
                }
            )
        )
        os.chmod(old_config, stat.S_IRUSR | stat.S_IWUSR)
        old_token = old_dir / "restore_token"
        old_token.write_text("tok")
        new_dir = tmp_path / "newcfg"

        with caplog.at_level(logging.INFO):
            loaded = load_config(base_dir=tmp_path, config_dir=new_dir)

        new_config = new_dir / "config.json"
        new_token = new_dir / "restore_token"
        assert loaded.server_url == "https://example.com"
        assert loaded.key == "test-key-123"
        assert loaded.stream == "archon"
        assert loaded.capture_framerate == 3
        assert new_config.exists()
        assert stat.S_IMODE(new_config.stat().st_mode) == 0o600
        assert new_token.read_text() == "tok"
        assert not old_config.exists()
        assert not old_token.exists()
        assert not old_dir.exists()
        migration_records = [
            record for record in caplog.records if "Migrated config" in record.message
        ]
        assert len(migration_records) == 1

        snapshot = (
            new_config.read_text(),
            new_token.read_text(),
            stat.S_IMODE(new_config.stat().st_mode),
        )
        caplog.clear()

        with caplog.at_level(logging.INFO):
            loaded_again = load_config(base_dir=tmp_path, config_dir=new_dir)

        assert loaded_again.capture_framerate == 3
        assert [
            record for record in caplog.records if "Migrated config" in record.message
        ] == []
        assert (
            new_config.read_text(),
            new_token.read_text(),
            stat.S_IMODE(new_config.stat().st_mode),
        ) == snapshot

    def test_no_migration_when_config_dir_is_legacy(self, tmp_path: Path):
        config_dir = tmp_path / "config"
        config_dir.mkdir()
        config_path = config_dir / "config.json"
        content = '{"server_url": "http://test", "capture_framerate": 4}'
        config_path.write_text(content)

        loaded = load_config(base_dir=tmp_path, config_dir=config_dir)

        assert loaded.server_url == "http://test"
        assert loaded.capture_framerate == 4
        assert config_path.exists()
        assert config_path.read_text() == content
