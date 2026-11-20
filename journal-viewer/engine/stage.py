#!/usr/bin/env python3
"""Stage only the redistributable portion of the private Jammy SDK."""
# SPDX-License-Identifier: AGPL-3.0-only
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile

here = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("dest", type=Path)
parser.add_argument("--source", type=Path, required=True, help="Verified and patched WebKit release source tree")
parser.add_argument("--sdk", type=Path, default=Path("/opt/solstone-webkit"))
args = parser.parse_args()
root = args.dest / "opt/solstone-webkit"
if root.exists():
    parser.error("destination already contains a viewer")
root.mkdir(parents=True)
libdir = Path("/usr/lib") / subprocess.check_output(["dpkg-architecture", "-qDEB_HOST_MULTIARCH"], text=True).strip()


def install(source, dest, mode=0o644):
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, dest)
    dest.chmod(mode)


for name in ("libwebkitgtk-6.0.so.4", "libjavascriptcoregtk-6.0.so.1"):
    install(args.sdk / "lib" / name, root / "lib" / name)
injected = Path("lib/webkitgtk-6.0/injected-bundle/libwebkitgtkinjectedbundle.so")
install(args.sdk / injected, root / injected)
for name in ("WebKitGPUProcess", "WebKitNetworkProcess", "WebKitWebProcess"):
    helper = Path("libexec/webkitgtk-6.0") / name
    install(args.sdk / helper, root / helper, 0o755)
for catalog in sorted((args.sdk / "share/locale").glob("*/LC_MESSAGES/WebKitGTK-6.0.mo")):
    install(catalog, root / catalog.relative_to(args.sdk))
for name in ("libicui18n.so.70", "libicuuc.so.70", "libicudata.so.70"):
    install(libdir / name, root / "lib" / name)
for name in ("libjpeg.so.8", "libxml2.so.2", "libmanette-0.2.so.0"):
    install(libdir / name, root / "fallback" / name / name)
doc = root / "licenses"
# Preserve notices from the exact WebKit source, including bundled third parties.
doc.mkdir(parents=True)
with (doc / "webkit-notices.tar.gz").open("wb") as output:
    with gzip.GzipFile(fileobj=output, mode="wb", mtime=0, filename="") as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as archive:
            for path in sorted((args.source / "Source").rglob("*")):
                if path.is_file() and path.name.lower().startswith(("license", "licence", "copying", "notice", "copyright")):
                    member = tarfile.TarInfo(str(path.relative_to(args.source)))
                    member.size = path.stat().st_size
                    with path.open("rb") as content:
                        archive.addfile(member, content)
for package in ("libicu70", "libjpeg-turbo8", "libxml2", "libmanette-0.2-0"):
    install(Path("/usr/share/doc") / package / "copyright", doc / (package + ".copyright"))
packages = ["libicu70", "libjpeg-turbo8", "libxml2", "libmanette-0.2-0"]
versions = subprocess.check_output(["dpkg-query", "-W", "-f=" + '${source:Package} ${source:Version}\\n', *packages], text=True)
(doc / "sources").write_text("https://webkitgtk.org/releases/webkitgtk-2.54.3.tar.xz\n"
    "Patches and build instructions: journal-viewer/engine in the matching solstone-linux source.\n"
    "Ubuntu source packages (archive.ubuntu.com or ports.ubuntu.com; apt-get source name=version):\n" + versions)
shutil.copytree(args.sdk / "include/webkitgtk-6.0", args.dest / "include/webkitgtk-6.0")
install(libdir / "libsoup-3.0.so.0", args.dest / "link/libsoup-3.0.so.0")
shutil.copytree("/usr/include/libsoup-3.0", args.dest / "include/libsoup-3.0")
files = {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
         for p in sorted(root.rglob("*")) if p.is_file()}
(root / "runtime.json").write_text(json.dumps({"webkit": "2.54.3", "files": files}, indent=2) + "\n")
