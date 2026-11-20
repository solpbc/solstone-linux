# SPDX-License-Identifier: AGPL-3.0-only
"""Generate the launcher for a single verified native engine and journal host."""
import hashlib
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent


def launcher(root):
    inventory = json.loads((root / "runtime.json").read_bytes())
    hashes = inventory["files"].copy()
    for name in ("runtime.json", "bin/journal"):
        hashes[name] = hashlib.sha256((root / name).read_bytes()).hexdigest()
    lines = []
    for name, digest in sorted(hashes.items()):
        if not name.isascii() or any(ch not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._/@" for ch in name):
            raise ValueError("invalid runtime filename")
        lines.append(f"{digest}  {name}")
    epoch = int((HERE / "engine/security-generation").read_text())
    if not 0 < epoch <= 2**31 - 1:
        raise ValueError("invalid security generation")
    template = (HERE / "launch").read_text()
    return template.replace("@JOURNAL_DIGESTS@", "\n".join(lines)).replace("@SECURITY_GENERATION@", str(epoch))
