#!/usr/bin/env python3
"""Bump the patch version in extension.toml, Cargo.toml, and package.json."""

import re

try:
    with open("extension.toml") as f:
        content = f.read()
    m = re.search(r'version = "(\d+)\.(\d+)\.(\d+)"', content)
    if not m:
        raise SystemExit("could not find version in extension.toml")
    major, minor, patch = int(m.group(1)), int(m.group(2)), int(m.group(3))
    patch += 1
    new_ver = f"{major}.{minor}.{patch}"
    content = re.sub(
        r'version = "[^"]+"',
        f'version = "{new_ver}"',
        content,
        count=1,
    )
    with open("extension.toml", "w") as f:
        f.write(content)
except FileNotFoundError:
    pass  # extension.toml may not exist in all contexts

# Cargo.toml
with open("Cargo.toml") as f:
    cargo = f.read()
cargo = re.sub(
    r'(?<=^version = ")[^"]+',
    new_ver,
    cargo,
    flags=re.MULTILINE,
)
with open("Cargo.toml", "w") as f:
    f.write(cargo)

# package.json
with open("package.json") as f:
    pkg = f.read()
pkg = re.sub(
    r'"version": "[^"]+"',
    f'"version": "{new_ver}"',
    pkg,
    count=1,
)
with open("package.json", "w") as f:
    f.write(pkg)

print(new_ver)
