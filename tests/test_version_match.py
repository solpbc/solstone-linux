# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def _read_assignment(path, name):
    pattern = re.compile(rf'^{re.escape(name)}\s*=\s*"([^"]+)"\s*$')
    for line in path.read_text().splitlines():
        match = pattern.match(line)
        if match:
            return match.group(1)
    raise AssertionError(f"{name} assignment not found in {path}")


def test_package_version_matches_project_version():
    project_version = _read_assignment(ROOT / "pyproject.toml", "version")
    package_version = _read_assignment(
        ROOT / "src" / "solstone_linux" / "__init__.py",
        "__version__",
    )

    assert package_version == project_version
