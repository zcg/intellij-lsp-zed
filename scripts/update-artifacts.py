#!/usr/bin/env python3
"""Download all platform vsixes and rebuild server-artifacts.json."""

import json
import os
import subprocess
import sys

VSIX_VERSION = sys.argv[1]
ARTIFACTS_PATH = sys.argv[2] if len(sys.argv) > 2 else "server-artifacts.json"

API_BASE = "https://open-vsx.org/api/JetBrains/intellij-server"
url_tpl = (
    f"{API_BASE}/{{plat}}/{VSIX_VERSION}"
    f"/file/JetBrains.intellij-server-{VSIX_VERSION}@{{plat}}.vsix"
)

platforms = {
    "darwin-x64": "mac-x86_64",
    "darwin-arm64": "mac-aarch64",
    "linux-x64": "linux-x86_64",
    "linux-arm64": "linux-aarch64",
    "win32-x64": "windows-x86_64",
    "win32-arm64": "windows-aarch64",
}

result: dict = {"version": "", "vsix_version": VSIX_VERSION, "platforms": {}}
server_version = None

for plat, key in platforms.items():
    url = url_tpl.format(plat=plat)
    vsix = f"/tmp/vsix-{plat}.zip"
    print(f"Downloading {plat} ...")
    subprocess.run(["curl", "-fsSL", "-o", vsix, url], check=True)
    bundle_raw = subprocess.check_output(
        ["unzip", "-p", vsix, "extension/server-bundle.json"]
    )
    bundle = json.loads(bundle_raw)
    if server_version is None:
        server_version = bundle["version"]
        result["version"] = server_version
    archive = bundle.get("archiveName", "")
    file_type = "gzip-tar" if archive.endswith(".tar.gz") else "zip"
    result["platforms"][key] = {
        "url": bundle["url"],
        "sha256": bundle.get("sha256", ""),
        "file_type": file_type,
    }
    os.remove(vsix)

with open(ARTIFACTS_PATH, "w") as f:
    json.dump(result, f, indent=2)
    f.write("\n")
print(f"Updated {ARTIFACTS_PATH} (server {result['version']}, vsix {VSIX_VERSION})")
