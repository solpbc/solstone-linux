#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Check a staged private engine against the release's reviewed native inventory."""
import argparse
import hashlib
import json
from pathlib import Path
import platform

from release import launcher

here = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("stage", type=Path)
args = parser.parse_args()
pin = json.loads((here / "engine" / (platform.machine() + ".json")).read_bytes())
root = args.stage / "opt/solstone-webkit"
actual = json.loads((root / "runtime.json").read_bytes())
if actual != pin:
    raise SystemExit("private journal runtime version or inventory differs from the release pin")
paths = {str(path.relative_to(root)) for path in root.rglob("*") if not path.is_dir()}
if paths != set(pin["files"]) | {"runtime.json", "bin/journal"}:
    raise SystemExit("incomplete or unexpected private journal payload")
for name, expected in pin["files"].items():
    path = root / name
    if path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        raise SystemExit("private journal payload digest mismatch: " + name)
if (args.stage / "usr/bin/solstone-linux-journal").read_text() != launcher(root):
    raise SystemExit("journal launcher is not bound to this engine and host")
policy = args.stage / "usr/share/solstone-linux/apparmor/solstone-linux-journal"
if policy.read_bytes() != (here / "apparmor").read_bytes():
    raise SystemExit("journal profile differs from source")
