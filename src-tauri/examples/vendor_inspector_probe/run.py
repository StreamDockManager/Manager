#!/usr/bin/env python3
"""Run the built probe in a temporary, CUA-addressable app bundle, never install it."""
import argparse
import os
import pathlib
import plistlib
import shutil
import subprocess
import tempfile
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--interactive", action="store_true")
parser.add_argument("--log", required=True, type=pathlib.Path)
args = parser.parse_args()
project = pathlib.Path(__file__).resolve().parents[3]
binary = project / "target/debug/examples/vendor_inspector_probe"
if not binary.is_file():
    raise SystemExit("Build the native-inspector-probe example first")
with tempfile.TemporaryDirectory(prefix="manager-native-probe-") as temporary:
    bundle = pathlib.Path(temporary) / "Disposable Inspector Probe.app"
    contents = bundle / "Contents"
    executable = contents / "MacOS/DisposableInspectorProbe"
    executable.parent.mkdir(parents=True)
    shutil.copy2(binary, executable)
    with (contents / "Info.plist").open("wb") as handle:
        plistlib.dump({
            "CFBundleExecutable": executable.name,
            "CFBundleIdentifier": "org.manager.disposable-probe." + uuid.uuid4().hex,
            "CFBundleName": "Disposable Inspector Probe",
            "CFBundlePackageType": "APPL",
            "CFBundleVersion": "1",
            "NSHighResolutionCapable": True,
        }, handle)
    fixture_temporary = pathlib.Path(temporary) / "fixtures"
    fixture_temporary.mkdir()
    environment = os.environ.copy()
    environment["TMPDIR"] = str(fixture_temporary) + os.sep
    with args.log.open("w") as log:
        command = [str(executable)] + (["--interactive"] if args.interactive else [])
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, env=environment)
        try:
            print("DISPOSABLE_APP", bundle, flush=True)
            print("DISPOSABLE_PROBE_PID", process.pid, flush=True)
            result = process.wait(timeout=360 if args.interactive else 45)
        except subprocess.TimeoutExpired:
            print("DISPOSABLE_PROBE_WATCHDOG_STOP", flush=True)
            result = 124
        finally:
            # Every exit path retires only our own child before deleting its bundle
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    output = args.log.read_text()
    print(output, end="", flush=True)
    for line in output.splitlines():
        if line.startswith("DISPOSABLE_FIXTURE_ROOT "):
            root = pathlib.Path(line.removeprefix("DISPOSABLE_FIXTURE_ROOT "))
            print("DISPOSABLE_FIXTURE_GRACEFULLY_REMOVED", not root.exists(), flush=True)
    # TemporaryDirectory owns the app and child-only TMPDIR, including any
    # fixture leftovers after forced exit. No path from stdout is deleted
    print("DISPOSABLE_PROBE_EXIT", result, flush=True)
print("DISPOSABLE_APP_REMOVED", not bundle.exists(), flush=True)
print("DISPOSABLE_CHILD_TMPDIR_REMOVED", not fixture_temporary.exists(), flush=True)
raise SystemExit(result)
