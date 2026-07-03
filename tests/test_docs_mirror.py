# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
DISTROS = {
    "fedora": "fedora",
    "debian / ubuntu": "debian / ubuntu",
    "arch": "arch",
    "opensuse": "opensuse",
}


def _normalize_command(lines):
    joined = " ".join(line.strip().rstrip("\\").strip() for line in lines)
    return re.sub(r"\s+", " ", joined).strip()


def _dependency_commands(path):
    lines = path.read_text().splitlines()
    commands = {}
    for index, line in enumerate(lines):
        match = re.match(r"\s*\*\*([^*]+):\*\*", line)
        if not match:
            continue
        key = DISTROS.get(match.group(1).strip().casefold())
        if key is None:
            continue
        if key in commands:
            continue

        fence_start = None
        for probe in range(index + 1, len(lines)):
            if lines[probe].strip() == "```":
                fence_start = probe + 1
                break
        assert fence_start is not None, f"missing command fence after {line!r}"

        command_lines = []
        for probe in range(fence_start, len(lines)):
            if lines[probe].strip() == "```":
                break
            command_lines.append(lines[probe])
        commands[key] = _normalize_command(command_lines)

    return commands


def test_readme_and_install_dependency_commands_match():
    readme = _dependency_commands(ROOT / "README.md")
    install = _dependency_commands(ROOT / "INSTALL.md")

    assert readme.keys() == install.keys() == DISTROS.keys()
    assert readme == install
