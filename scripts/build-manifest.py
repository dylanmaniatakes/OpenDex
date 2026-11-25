#!/usr/bin/env python3
import datetime
import json
import pathlib
import platform
import subprocess
import sys

if len(sys.argv) != 5:
    raise SystemExit("usage: build-manifest.py TARGET BUNDLE VERSION PROFILE")

destination = pathlib.Path(sys.argv[2])
executable = destination / "Contents/MacOS/opendex"
if not executable.is_file():
    raise SystemExit(f"No OpenDex executable found at {executable}")


def command(value):
    try:
        result = subprocess.run(value, check=True, capture_output=True, text=True)
        return (result.stdout + result.stderr).strip()
    except (OSError, subprocess.SubprocessError):
        return None


def git_value(value):
    try:
        result = subprocess.run(value, check=True, capture_output=True, text=True)
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout.strip() or None

commit = git_value(["git", "rev-parse", "HEAD"])
in_worktree = git_value(["git", "rev-parse", "--is-inside-work-tree"])
dirty = False
if in_worktree == "true":
    status = subprocess.run(["git", "status", "--porcelain"], capture_output=True, text=True)
    dirty = status.returncode != 0 or bool(status.stdout.strip())

sha256 = (command(["shasum", "-a", "256", str(executable)]) or "").split()[0]
codesign_output = command(["codesign", "--display", "--verbose=4", str(destination)]) or ""
identity = "adhoc" if "Signature=adhoc" in codesign_output else "unknown"
cdhash = next((line.split("=", 1)[1].strip() for line in codesign_output.splitlines() if line.startswith("CDHash=")), None)

path = pathlib.Path(sys.argv[1])
manifest = {
    "artifact": {
        "name": "OpenDex.app",
        "version": sys.argv[3],
        "profile": sys.argv[4],
        "commit": commit,
        "dirty": dirty,
    },
    "bundle": {
        "path": destination.name,
        "identifier": command(["/usr/libexec/PlistBuddy", "-c", "Print :CFBundleIdentifier", str(destination / "Contents/Info.plist")]),
        "signature": identity,
        "signed": command(["codesign", "--verify", "--strict", str(destination)]) == "",
        "cdhash": cdhash,

        "executable_sha256": sha256,
        "size_bytes": executable.stat().st_size,
    },
    "toolchain": {
        "cargo": command(["cargo", "--version"]),
        "swiftc": command(["swiftc", "--version"]),
    },
    "host": {"os": platform.system(), "arch": platform.machine()},
    "built_at": datetime.datetime.now(datetime.UTC).isoformat(),
}
path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
