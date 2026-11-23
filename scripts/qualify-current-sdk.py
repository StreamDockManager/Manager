#!/usr/bin/env python3
"""Qualify the production 7.1 subset or an isolated candidate without changing claims."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
EXCLUDED = {".git", "node_modules", "target", "artifacts", "dist", ".DS_Store", "__pycache__", ".cache"}


# Calculate the file content digest used for integrity checks
def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


# Check the current SDK qualification gates before creating fixtures
def audit_gates(fixture):
    sdk = ROOT / fixture / "node_modules/@elgato/streamdeck"
    if json.loads((sdk / "package.json").read_text())["version"] != "3.0.4":
        raise RuntimeError("Review the SDK audit before changing its pinned version")
    pattern = re.compile(r'requiresVersion\((7\.[01]),\s*connection\.version,\s*"([^"]+)"\)')
    gates = sorted(
        (str(path.relative_to(sdk)), version, feature)
        for path in (sdk / "dist").rglob("*.js")
        for version, feature in pattern.findall(path.read_text())
    )
    expected = sorted([
        ("dist/plugin/actions/action-base.js", "7.1", "getResources"),
        ("dist/plugin/actions/action-base.js", "7.1", "setResources"),
        ("dist/plugin/devices/service.js", "7.0", "onDeviceDidChange"),
        ("dist/plugin/settings.js", "7.1", "Default onDidReceiveSettings/onDidReceiveGlobalSettings behavior"),
    ])
    if gates != expected:
        raise RuntimeError("The upstream 7.0/7.1 gate inventory changed; review before qualification")
    return {
        "explicit_gates": gates,
        "scanned_files_sha256": {
            str(path.relative_to(sdk)): digest(path)
            for path in sorted((sdk / "dist").rglob("*"))
            if path.is_file() and path.suffix in {".js", ".ts"}
        },
    }


# Stage the current SDK fixtures after checking required qualification gates
def prepare(destination, include_profiles=False, production=False):
    config_path = Path("crates/plugin-host/connection-contract.v1.json")
    registry_path = Path("crates/plugin-host/connection-implementations.v1.json")
    originals = {str(path): digest(ROOT / path) for path in [config_path, registry_path]}
    source_config = json.loads((ROOT / config_path).read_text())
    expected = ("7.1.0", "streamdeck-7.1-subset") if production else ("6.9.0", "streamdeck-6.9-subset")
    vendor = source_config["vendors"]["streamdeck"]
    if (vendor["application_version"], vendor["compatibility_label"]) != expected:
        raise RuntimeError("Source declaration does not match the selected qualification mode; use --production for the checked-in 7.1 subset")
    fixture = Path("tests/fixtures/plugins/elgato-sdk-executable")
    sdk_audit = audit_gates(fixture)
    bundle = fixture / "package/com.manager.fixture.elgato.sdPlugin/current-mode.js"
    if not (ROOT / bundle).is_file():
        raise RuntimeError("Run npm run build:current in the existing locked official SDK fixture first")
    bundles = [bundle]
    if include_profiles:
        profile_bundle = fixture / "package/com.manager.fixture.elgato.sdPlugin/bundled-profiles.js"
        if not (ROOT / profile_bundle).is_file():
            raise RuntimeError("Run npm run build:profiles in the existing locked official SDK fixture first")
        bundles.append(profile_bundle)
    snapshot = destination / "source"
    snapshot.mkdir()
    # Explicit source inputs plus already-verified patched dependencies. Never
    # copy archive caches, preparation locks, temporary stages or other caches
    pinned_sources = {f"{d['name']}-{d['version']}" for d in json.loads((ROOT / 'vendor/dependencies.json').read_text())['dependencies']}
    for item in ["scripts/prepare_patched_deps.py", "patches", "Cargo.toml", "Cargo.lock", "crates", "hardware", "vendor", "xtask", "src-tauri/Cargo.toml", "src-tauri/build.rs", "src-tauri/src", "src-tauri/examples/vendor_inspector_probe.rs", "public/appearance/icons", "public/appearance/fonts", "src/palette.v1.json", "src/device-palettes.v1.json", "src/builtin-catalog.json", "index.html", "tests"]:
        source = ROOT / item
        if source.is_symlink():
            raise RuntimeError(f"Refusing symlink source input: {item}")
        if source.is_file():
            (snapshot / item).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, snapshot / item)
            continue
        for directory, dirs, files in os.walk(source, followlinks=False):
            parent = Path(directory)
            dirs[:] = [name for name in dirs if
                       (name not in EXCLUDED or (parent == ROOT / 'vendor' and name == '.cache'))
                       and not (parent / name).is_symlink()]
            if parent == ROOT / 'vendor/.cache':
                dirs[:] = [name for name in dirs if name == 'sources']
                files = []
            if parent == ROOT / 'vendor/.cache/sources':
                dirs[:] = [name for name in dirs if name in pinned_sources]
                files = []
            relative = parent.relative_to(ROOT)
            (snapshot / relative).mkdir(parents=True, exist_ok=True)
            for name in files:
                path = parent / name
                if name not in EXCLUDED and not path.is_symlink():
                    shutil.copy2(path, snapshot / relative / name)
    files = {str(path.relative_to(snapshot)): digest(path) for path in sorted(snapshot.rglob("*")) if path.is_file()}
    manifest = {
        "scope": "Production subset source audit; tests run directly in the source checkout" if production else "Candidate source snapshot, not a supported or releasable build",
        "production_mode": production,
        "source_files_sha256": files,
        "snapshot_sha256": hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest(),
        "candidate_only_changes": [] if production else [str(config_path), str(registry_path)],
        "production_contract_sha256": originals,
        "sdk_package_lock_sha256": digest(snapshot / fixture / "package-lock.json"),
        "probe_bundle_sha256": digest(snapshot / bundle),
        "probe_bundles_sha256": {str(path): digest(snapshot / path) for path in bundles},
        "runtime_profile_candidate_enabled": include_profiles,
        "runner_sha256": digest(Path(__file__)),
        "sdk_source_audit": sdk_audit,
        "full_compatibility_claim": False,
    }
    if include_profiles:
        profile_fixture = Path("tests/fixtures/plugins/vendor/elgato/lights-out-profiles")
        manifest["authentic_profile_inputs_sha256"] = {
            str(path): digest(snapshot / path)
            for path in [profile_fixture / "manifest.json", profile_fixture / "sd.streamDeckProfile"]
        }
    if not production:
        config = json.loads((snapshot / config_path).read_text())
        config["vendors"]["streamdeck"]["application_version"] = "7.1.0"
        config["vendors"]["streamdeck"]["compatibility_label"] = "streamdeck-7.1-candidate-subset"
        registry = json.loads((snapshot / registry_path).read_text())
        for entry in registry["vendors"]:
            if entry["dialect"] == "streamdeck":
                entry["application_version"] = "7.1.0"
                entry["compatibility_label"] = "streamdeck-7.1-candidate-subset"
        for path, value in [(config_path, config), (registry_path, registry)]:
            (snapshot / path).write_text(json.dumps(value, indent=4) + "\n")
    manifest["candidate_files_sha256"] = {str(path): digest(snapshot / path) for path in [config_path, registry_path]}
    (destination / "snapshot.json").write_text(json.dumps(manifest, indent=2) + "\n")
    assert originals == {str(path): digest(ROOT / path) for path in [config_path, registry_path]}
    return snapshot, manifest


# Parse options and run the qualify current sdk workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--production", action="store_true", help="Test checked-in production source directly; never inject candidate versions")
    parser.add_argument("--run", action="store_true", help="Run the ignored candidate tests after preparation")
    parser.add_argument("--runtime", action="store_true", help="Also run genuine SDK-to-real Runtime durability and bundled-profile review/switch tests")
    parser.add_argument("--node", type=Path, help="Explicit existing Node 24 executable; never installed by this script")
    args = parser.parse_args()
    if args.runtime and not args.run:
        parser.error("--runtime requires --run")
    if args.run and (not args.node or not args.node.is_absolute() or not args.node.is_file()):
        parser.error("--run requires --node with an existing absolute Node 24 path")
    parent = ROOT / "artifacts/current-sdk-candidate"
    parent.mkdir(parents=True, exist_ok=True)
    destination = Path(tempfile.mkdtemp(prefix="production-" if args.production else "snapshot-", dir=parent))
    # Warm explicitly beforehand; qualification itself must remain offline
    subprocess.run([sys.executable, str(ROOT / 'scripts/prepare_patched_deps.py'), '--offline'], cwd=ROOT, check=True)
    snapshot, manifest = prepare(destination, args.runtime, args.production)
    print(f"Source audit snapshot: {snapshot}", flush=True)
    print(f"Source SHA256: {manifest['snapshot_sha256']}", flush=True)
    print("Production contract and implementation registry are unchanged. Full release gate remains independent.", flush=True)
    if not args.run:
        return
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(parent / "target")
    env["STREAM_DOCK_MANAGER_NODE"] = str(args.node)
    for key in ["STREAM_DOCK_MANAGER_CANDIDATE_QUALIFICATION", "STREAM_DOCK_MANAGER_PRODUCTION_QUALIFICATION"]:
        env.pop(key, None)
    env["STREAM_DOCK_MANAGER_PRODUCTION_QUALIFICATION" if args.production else "STREAM_DOCK_MANAGER_CANDIDATE_QUALIFICATION"] = manifest["snapshot_sha256"]
    commands = [("host-transport", ["cargo", "test", "--jobs", "2", "--offline", "--locked", "-p", "stream-dock-manager-plugin-host", "--test", "current_sdk_candidate", "--", "--ignored", "--test-threads=1", "--nocapture"])]
    if args.runtime:
        commands.append(("real-runtime-durability", ["cargo", "test", "--jobs", "2", "--offline", "--locked", "-p", "stream-dock-manager-runtime", "--features", "test-support", "--test", "current_sdk_runtime", "--", "--ignored", "--test-threads=1", "--nocapture"]))
    # This workflow explicitly runs offline; preparation must not fetch either
    execution_root = ROOT if args.production else snapshot
    subprocess.run([sys.executable, str(execution_root / 'scripts/prepare_patched_deps.py'), '--offline'], cwd=execution_root, env=env, check=True)
    stages = []
    with (destination / "test-output.txt").open("w") as log:
        for name, command in commands:
            print(f"Running qualification stage: {name}", flush=True)
            log.write(f"\n=== {name} ===\n")
            log.flush()
            result = subprocess.run([sys.executable, str(ROOT / "scripts/with_test_key.py"), *command], cwd=execution_root, env=env, stdout=log, stderr=subprocess.STDOUT, check=False)
            stages.append({"stage": name, "exit_code": result.returncode})
            print(f"Qualification stage {name}: exit {result.returncode}", flush=True)
    print((destination / "test-output.txt").read_text(), end="")
    unchanged = all(digest(ROOT / path) == expected for path, expected in manifest["production_contract_sha256"].items())
    profile_inputs_unchanged = all(digest(ROOT / path) == expected for path, expected in manifest.get("authentic_profile_inputs_sha256", {}).items())
    exit_code = next((stage["exit_code"] for stage in stages if stage["exit_code"]), 0)
    source_unchanged = all((ROOT / path).is_file() and digest(ROOT / path) == expected for path, expected in manifest["source_files_sha256"].items())
    (destination / "result.json").write_text(json.dumps({"exit_code": exit_code, "stages": stages, "source_sha256": manifest["snapshot_sha256"], "production_mode": args.production, "execution_directory": str(ROOT if args.production else snapshot), "source_inputs_unchanged": source_unchanged, "production_contract_unchanged": unchanged, "authentic_profile_inputs_unchanged": profile_inputs_unchanged if args.runtime else None, "full_compatibility_claim": False}, indent=2) + "\n")
    if not unchanged:
        raise RuntimeError("Production contract changed independently during this candidate run; review before interpreting results")
    if not profile_inputs_unchanged:
        raise RuntimeError("Authentic profile inputs changed during qualification; review before interpreting results")
    if args.production and not source_unchanged:
        raise RuntimeError("Production source inputs changed during direct qualification; review before interpreting results")
    raise SystemExit(exit_code)


if __name__ == "__main__":
    main()
