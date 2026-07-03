# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Configuration loading and persistence for solstone-linux.

Config lives at ~/.config/solstone-linux/config.json.
Captures go to ~/.local/share/solstone-linux/captures/.
Screencast restore token at ~/.config/solstone-linux/restore_token.
"""

from __future__ import annotations

import json
import logging
import os
import shutil
import stat
from dataclasses import dataclass, field
from pathlib import Path

logger = logging.getLogger(__name__)

DEFAULT_BASE_DIR = Path.home() / ".local" / "share" / "solstone-linux"
DEFAULT_SERVER_URL = "http://localhost:5015"
DEFAULT_SEGMENT_INTERVAL = 300
DEFAULT_SYNC_RETRY_DELAYS = [5, 30, 120, 300]
DEFAULT_SYNC_MAX_RETRIES = 10
DEFAULT_SYNC_STALE_THRESHOLD = 600
INVALID_CONFIG_VALUE_WARNING = "Invalid config value for %s=%r; using default %r"


def _default_config_dir() -> Path:
    xdg = os.environ.get("XDG_CONFIG_HOME")
    if xdg:
        base = Path(xdg)
        if base.is_absolute():
            return base / "solstone-linux"
    return Path.home() / ".config" / "solstone-linux"


@dataclass
class Config:
    """Configuration for the Linux desktop observer."""

    server_url: str = ""
    key: str = ""
    stream: str = ""
    segment_interval: int = DEFAULT_SEGMENT_INTERVAL
    sync_retry_delays: list[int] = field(
        default_factory=lambda: list(DEFAULT_SYNC_RETRY_DELAYS)
    )
    sync_max_retries: int = DEFAULT_SYNC_MAX_RETRIES
    sync_stale_threshold: int = DEFAULT_SYNC_STALE_THRESHOLD
    cache_retention_days: int = 7
    chat_bridge_enabled: bool = True
    capture_framerate: int = 1
    draw_cursor: bool = True
    start_paused: bool = False
    base_dir: Path = DEFAULT_BASE_DIR
    config_dir: Path = field(default_factory=_default_config_dir)

    @property
    def captures_dir(self) -> Path:
        return self.base_dir / "captures"

    @property
    def state_dir(self) -> Path:
        return self.base_dir / "state"

    @property
    def config_path(self) -> Path:
        return self.config_dir / "config.json"

    @property
    def restore_token_path(self) -> Path:
        return self.config_dir / "restore_token"

    def ensure_dirs(self) -> None:
        """Create all required directories."""
        self.captures_dir.mkdir(parents=True, exist_ok=True)
        self.config_dir.mkdir(parents=True, exist_ok=True)
        self.state_dir.mkdir(parents=True, exist_ok=True)


def _migrate_legacy_config(config: Config) -> None:
    old_dir = config.base_dir / "config"
    if config.config_dir == old_dir:
        return
    if config.config_path.exists():
        return
    old_config = old_dir / "config.json"
    if not old_config.exists():
        return
    try:
        config.config_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(old_config, config.config_path)
        os.chmod(config.config_path, stat.S_IRUSR | stat.S_IWUSR)
        old_token = old_dir / "restore_token"
        if old_token.exists():
            shutil.copy2(old_token, config.restore_token_path)
        logger.info(f"Migrated config to {config.config_dir}")
    except OSError as e:
        logger.warning(f"Config migration failed: {e}")
        return
    for p in (old_config, old_dir / "restore_token"):
        try:
            p.unlink()
        except OSError:
            pass
    try:
        old_dir.rmdir()
    except OSError:
        pass


def _load_int(data: dict, key: str, default: int) -> int:
    if key not in data:
        return default
    value = data[key]
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return int(value)
    logger.warning(INVALID_CONFIG_VALUE_WARNING, key, value, default)
    return default


def _load_int_list(data: dict, key: str, default: list[int]) -> list[int]:
    if key not in data:
        return list(default)
    value = data[key]
    if isinstance(value, list) and all(
        isinstance(item, (int, float)) and not isinstance(item, bool) for item in value
    ):
        return [int(item) for item in value]
    logger.warning(INVALID_CONFIG_VALUE_WARNING, key, value, default)
    return list(default)


def load_config(base_dir: Path | None = None, config_dir: Path | None = None) -> Config:
    """Load config from disk, returning defaults if not found."""
    config = Config()
    if base_dir:
        config.base_dir = base_dir
    if config_dir:
        config.config_dir = config_dir
    _migrate_legacy_config(config)

    config_path = config.config_path
    if not config_path.exists():
        return config

    try:
        with open(config_path, encoding="utf-8") as f:
            data = json.load(f)
    except (json.JSONDecodeError, OSError) as e:
        logger.warning(f"Failed to load config from {config_path}: {e}")
        return config

    if not isinstance(data, dict):
        logger.warning(f"Config at {config_path} is not a JSON object; using defaults")
        return config

    config.server_url = data.get("server_url", "")
    config.key = data.get("key", "")
    config.stream = data.get("stream", "")
    config.segment_interval = _load_int(
        data, "segment_interval", DEFAULT_SEGMENT_INTERVAL
    )
    config.sync_retry_delays = _load_int_list(
        data, "sync_retry_delays", config.sync_retry_delays
    )
    config.sync_max_retries = _load_int(
        data, "sync_max_retries", config.sync_max_retries
    )
    config.sync_stale_threshold = _load_int(
        data, "sync_stale_threshold", DEFAULT_SYNC_STALE_THRESHOLD
    )
    config.cache_retention_days = _load_int(data, "cache_retention_days", 7)
    config.chat_bridge_enabled = data.get("chat_bridge_enabled", True)
    config.capture_framerate = max(1, min(_load_int(data, "capture_framerate", 1), 10))
    config.draw_cursor = bool(data.get("draw_cursor", True))
    config.start_paused = bool(data.get("start_paused", False))

    return config


def save_config(config: Config) -> None:
    """Save config to disk with user-only permissions."""
    config.ensure_dirs()

    data = {
        "server_url": config.server_url,
        "key": config.key,
        "stream": config.stream,
        "segment_interval": config.segment_interval,
        "sync_retry_delays": config.sync_retry_delays,
        "sync_max_retries": config.sync_max_retries,
        "sync_stale_threshold": config.sync_stale_threshold,
        "cache_retention_days": config.cache_retention_days,
        "chat_bridge_enabled": config.chat_bridge_enabled,
        "capture_framerate": config.capture_framerate,
        "draw_cursor": config.draw_cursor,
        "start_paused": config.start_paused,
    }

    config_path = config.config_path
    tmp_path = config_path.with_suffix(f".{os.getpid()}.tmp")

    with open(tmp_path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.write("\n")

    # Set user-only read/write before moving into place
    os.chmod(tmp_path, stat.S_IRUSR | stat.S_IWUSR)
    os.rename(str(tmp_path), str(config_path))
    logger.info(f"Config saved to {config_path}")
