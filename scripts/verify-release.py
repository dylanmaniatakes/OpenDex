#!/usr/bin/env python3
import json
import pathlib
import platform
import subprocess
import sys

if len(sys.argv) > 2:
    raise SystemExit("usage: verify-release.py [TARGET_BUNDLE]")

repo = pathlib.Path(__file__).resolve().parent.parent
config = json.loads((repo / "tauri.conf.json").read_text(encoding="utf-8"))
bundle = (pathlib.Path(sys.argv[1]) if len(sys.argv) == 2 else repo / "dist" / "OpenDex.app").resolve()
manifest_path = bundle.parent / "OpenDex-build.json"
executable = bundle / "Contents/MacOS/opendex"

errors = []
def check(condition, message):
    if not condition:
        errors.append(message)

def run(value):
    return subprocess.run(value, capture_output=True, text=True)

check(bundle.is_dir(), f"Missing bundle: {bundle}")
check(executable.is_file(), f"Missing executable: {executable}")
check(manifest_path.is_file(), f"Missing build manifest: {manifest_path}")
if errors:
    raise SystemExit("\n".join(errors))

manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
version_result = run(["/usr/libexec/PlistBuddy", "-c", "Print :CFBundleShortVersionString", str(bundle / "Contents/Info.plist")])
identifier_result = run(["/usr/libexec/PlistBuddy", "-c", "Print :CFBundleIdentifier", str(bundle / "Contents/Info.plist")])
codesign_verify = run(["codesign", "--verify", "--strict", str(bundle)])
sha256_result = run(["shasum", "-a", "256", str(executable)])
try:
    git_result = run(["git", "rev-parse", "HEAD"])
    current_commit = git_result.stdout.strip() if git_result.returncode == 0 else None
except (OSError, subprocess.SubprocessError):
    current_commit = None

sha256 = sha256_result.stdout.strip().split()[0] if sha256_result.returncode == 0 and sha256_result.stdout else None
bundle = bundle.resolve()
artifact = manifest.get("artifact", {})
release_bundle = manifest.get("bundle", {})

for name, passed in [
    ("bundle version matches tauri configuration", version_result.returncode == 0 and version_result.stdout.strip() == config.get("version")),
    ("bundle identifier matches tauri configuration", identifier_result.returncode == 0 and identifier_result.stdout.strip() == config.get("identifier")),
    ("build profile is release", artifact.get("profile") == "release"),
    ("manifest version matches configuration", artifact.get("version") == config.get("version")),
    ("manifest commit matches current repository", artifact.get("commit") == current_commit),
    ("manifest executable size matches disk", release_bundle.get("size_bytes") == executable.stat().st_size),
    ("manifest executable hash matches disk", release_bundle.get("executable_sha256") == sha256),
    ("bundle ad-hoc signature verified", codesign_verify.returncode == 0),
    ("manifest signature state", release_bundle.get("signature") == "adhoc"),
    ("bundle host architecture matches host", manifest.get("host", {}).get("arch") == platform.machine()),
]:
    check(passed, name)

for key in ["identifier", "signature", "cdhash", "executable_sha256", "size_bytes"]:
    check(key in release_bundle, f"Missing manifest bundle field: {key}")
check("bundle path matches repository layout", bundle == (repo / "dist" / "OpenDex.app").resolve())

if errors:
    print("Release verification failed:")
    print("\n".join(f"- {message}" for message in errors))
    raise SystemExit(1)

print("Release verified.")
print(f"version: {version_result.stdout.strip()}")
print(f"identifier: {identifier_result.stdout.strip()}")
print(f"commit: {artifact.get('commit')}")
print(f"cdhash: {release_bundle.get('cdhash')}")
print(f"executable sha256: {sha256}")
