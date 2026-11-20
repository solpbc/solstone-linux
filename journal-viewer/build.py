#!/usr/bin/env python3
"""Build the GTK journal host against the pinned, private WebKit SDK."""
import argparse
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
contract = json.loads((here / "host-contract.json").read_bytes())
if contract["version"] != 1:
    raise SystemExit("unsupported journal host contract")
product, version = contract["user_agent_product"].split("/")
# The native tool image contains the matching WebKit and Soup headers.
# Only the UI process links WebKit. GStreamer, Mesa and the desktop's GTK
# are resolved on the owner's system and are intentionally outside the SDK.
flags = shlex.split(subprocess.check_output(
    ["pkg-config", "--cflags", "--libs", "gtk4"], text=True
))
flags.extend(["-I", str(options.sdk / "include/webkitgtk-6.0"),
              "-I", str(options.sdk / "include/libsoup-3.0"),
              "-L", str(options.sdk / "opt/solstone-webkit/lib"),
              "-L", str(options.sdk / "link"),
              "-Wl,--allow-shlib-undefined", "-l:libwebkitgtk-6.0.so.4", "-l:libsoup-3.0.so.0"])
with tempfile.TemporaryDirectory() as tmp:
    values = {"HOST_PRODUCT": product, "HOST_VERSION": version,
              "HOST_INIT": contract["initialization_script"]}
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
