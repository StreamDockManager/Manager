#!/usr/bin/env python3
"""Compile the actual metric algorithm without unrelated runtime dependencies.

External wire DTOs, serialization/schema annotations, and the runtime allocator
probe module are removed. The algorithm and standalone behavioral tests remain
unchanged. rustc -O is the optimized/release-mode
comparison; this is a CPU microbenchmark, not a Manager or hardware FPS result.
"""
import argparse
import hashlib
import json
import pathlib
import subprocess
import tempfile


# Parse options and run the metrics summary workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=pathlib.Path, help="Manager checkout")
    parser.add_argument("--output", required=True, type=pathlib.Path, help="New JSONL evidence path")
    parser.add_argument("--tests", action="store_true", help="Run the actual module tests instead")
    parser.add_argument("--repetitions", type=int, default=5, help="1..100, use 1 for alternating paired runs")
    args = parser.parse_args()
    if not 1 <= args.repetitions <= 100:
        parser.error("repetitions must be in 1..100")
    source = args.source / "crates/runtime/src/metrics.rs"
    original = source.read_text()
    replacements = {
        "use serde::{Deserialize, Serialize};\n": "",
        "Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema": "Debug, Clone",
    }
    standalone = original
    transformations = list(replacements)
    wire_start = standalone.find("#[derive(Serialize, Deserialize, ts_rs::TS)]\npub struct PerformanceCache")
    if wire_start >= 0:
        wire_end = standalone.index("const MAX_SERIES:", wire_start)
        if "pub struct PerformanceReport" not in standalone[wire_start:wire_end]:
            raise SystemExit("Unrecognized performance DTO layout; review transformation")
        standalone = standalone[:wire_start] + standalone[wire_end:]
        transformations.append("remove unrelated PerformanceCache/PerformanceReport DTOs")
    probe = '#[cfg(test)]\n#[path = "metrics_memory_tests.rs"]\nmod memory_tests;\n'
    if probe in standalone:
        standalone = standalone.replace(probe, "", 1)
        transformations.append("omit runtime-allocator-dependent memory probe module")
    for old, new in replacements.items():
        if standalone.count(old) != 1:
            raise SystemExit("Unrecognized derive/import layout; review transformation before running")
        standalone = standalone.replace(old, new)
    metadata = {"kind": "environment", "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
                "transformed_source_sha256": hashlib.sha256(standalone.encode()).hexdigest(),
                "flags": ["-O", "--edition=2021"], "sample_order": "(i*7919)%20000",
                "sample_window_seconds": 300, "clock": "std::time::Instant", "units": "nanoseconds",
                "category": "standalone_actual_metrics_algorithm", "hardware_fps": None,
                "transformations": transformations, "repetitions": args.repetitions}
    with tempfile.TemporaryDirectory(prefix="stream-dock-manager-metrics-") as directory:
        root = pathlib.Path(directory)
        (root / "metrics.rs").write_text(standalone)
        (root / "bench.rs").write_bytes(pathlib.Path(__file__).with_suffix(".rs").read_bytes())
        command = ["rustc", "-O", "--edition=2021", "-A", "dead_code"]
        if args.tests:
            command += ["--test", str(root / "metrics.rs")]
        else:
            command += [str(root / "bench.rs")]
        command += ["-o", str(root / "bench")]
        subprocess.run(command, check=True)
        # An existing report is never silently overwritten
        with args.output.open("x") as output:
            output.write(json.dumps(metadata) + "\n")
            output.flush()
            subprocess.run([str(root / "bench")] + ([] if args.tests else [str(args.repetitions)]), stdout=output, check=True)


if __name__ == "__main__":
    main()
