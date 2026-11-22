#!/usr/bin/env python3
"""Build the GTK journal host against the pinned, private WebKit SDK."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile

from release import launcher

here = Path(__file__).resolve().parent
args = argparse.ArgumentParser(description=__doc__)
args.add_argument("output", type=Path)
args.add_argument("--launcher", type=Path, help="Generate the installed launcher beside the daemon")
args.add_argument("--sdk", type=Path, default=Path("/opt/solstone-journal-dist"))
options = args.parse_args()
import_bytes = (here / "import-host-contract.json").read_bytes()
if hashlib.sha256(import_bytes).hexdigest() != "f346f9b9dbb09f7390dfa775ea7eeaf70f630080a3fc6149968dae1e8b39686d":
    raise SystemExit("journal import host contract differs from its pin")
import_contract = json.loads(import_bytes)
auth_contract = json.loads((here / "auth-host-contract.json").read_bytes())
if auth_contract["version"] != 2 or auth_contract["capability"] != import_contract["capability"]:
    raise SystemExit("unsupported journal auth host contract")
contract = json.loads((here / "host-contract.json").read_bytes())
if contract["version"] != 1:
    raise SystemExit("unsupported journal host contract")
product, version = contract["user_agent_product"].split("/")
# The native tool image contains the matching WebKit and Soup headers.
# Only the UI process links WebKit. GStreamer, Mesa and the desktop's GTK
# are resolved on the owner's system and are intentionally outside the SDK.
flags = shlex.split(subprocess.check_output(
    ["pkg-config", "--cflags", "--libs", "gtk4", "gio-unix-2.0"], text=True
))
flags.extend(["-I", str(options.sdk / "include/webkitgtk-6.0"),
              "-I", str(options.sdk / "include/libsoup-3.0"),
              "-L", str(options.sdk / "opt/solstone-webkit/lib"),
              "-L", str(options.sdk / "link"),
              "-Wl,--allow-shlib-undefined", "-l:libwebkitgtk-6.0.so.4", "-l:libjavascriptcoregtk-6.0.so.1", "-l:libsoup-3.0.so.0"])
with tempfile.TemporaryDirectory() as tmp:
    values = {"IMPORT_CONTEXT_LIMIT": import_contract["contextMaxBytes"],
              "IMPORT_PICK": import_contract["pick"], "IMPORT_REPLY": import_contract["reply"], "HOST_PRODUCT": product, "HOST_VERSION": version,
              "HOST_INIT": contract["initialization_script"],
              "IMPORT_CONTRACT": json.dumps(import_contract),
              "AUTH_CONTRACT": json.dumps(auth_contract),
              "AUTH_PREPARE": auth_contract["prepare"],
              "AUTH_PREPARED": auth_contract["prepared"],
              "AUTH_AUTHORIZE": auth_contract["authorize"],
              "AUTH_CANCEL": auth_contract["cancel"],
              "AUTH_RESULT": auth_contract["result"],
              "IMPORT_SCRIPT": (here / "import-host.js").read_text()}
    (Path(tmp) / "host-contract.h").write_text("".join(
        f"#define {name} {json.dumps(value)}\n" for name, value in values.items()
    ))
    options.output.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run([os.environ.get("CC", "cc"), "-O2", "-Wall", "-Wextra", "-Werror",
                    "-Wl,-z,relro,-z,now", "-fstack-protector-strong", "-D_FORTIFY_SOURCE=2",
                    "-I", tmp, str(here / "journal.c"), *flags,
                    "-o", str(options.output)], check=True)

if options.launcher:
    options.launcher.parent.mkdir(parents=True, exist_ok=True)
    options.launcher.write_text(launcher(options.output.parent.parent))
    options.launcher.chmod(0o755)
