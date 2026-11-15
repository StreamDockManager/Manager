#!/usr/bin/env python3
"""Verify pinned VSD retail identity evidence without loading vendor code.

Reads only the manufacturer app's Mach-O bytes and Info.plist. The checked-in
VM addresses come from reviewed arm64 VpidToUUID::datasheet disassembly; they
are valid only for the exact hashed binary, not a general discovery heuristic.
"""
import argparse
import hashlib
import json
import plistlib
import struct
from pathlib import Path


# Read ARM64 binary segment sizes from the inspected executable
def arm64_segments(data):
    base = 0
    if data[:4] == bytes.fromhex("cafebabe"):
        count = struct.unpack_from(">I", data, 4)[0]
        candidates = [struct.unpack_from(">IIIII", data, 8 + i * 20) for i in range(count)]
        base = next(row[2] for row in candidates if row[0] == 0x0100000C)
    if struct.unpack_from("<II", data, base) != (0xFEEDFACF, 0x0100000C):
        raise ValueError("expected arm64 Mach-O")
    count = struct.unpack_from("<I", data, base + 16)[0]
    position = base + 32
    segments = []
    for _ in range(count):
        command, length = struct.unpack_from("<II", data, position)
        if command == 0x19:
            _, address, _, offset, size = struct.unpack_from("<16sQQQQ", data, position + 8)
            segments.append((address, size, base + offset))
        position += length
    return segments


# Calculate virtual-memory bytes from the inspected segment metadata
def vm_bytes(data, segments, address, size):
    start, _, offset = next(row for row in segments if row[0] <= address and address + size <= row[0] + row[1])
    return data[offset + address - start:offset + address - start + size]


# Inspect the selected artifact or owned container and return its metadata
def inspect(app, evidence):
    version = plistlib.loads((app / "Contents/Info.plist").read_bytes())["CFBundleShortVersionString"]
    if version != evidence["manufacturer_app"]["version"]:
        raise ValueError("unreviewed manufacturer app version")
    binaries = {}
    for source in evidence["manufacturer_app"]["binaries"]:
        data = (app / "Contents" / source["path"]).read_bytes()
        if hashlib.sha256(data).hexdigest() != source["sha256"]:
            raise ValueError(f"unreviewed binary bytes: {source['path']}")
        binaries[source["path"]] = data
    data = binaries["MacOS/VSD Craft"]
    segments = arm64_segments(data)
    rows = []
    for row in evidence["registrations"]:
        name = row["manufacturer_name"]
        encoded = name.encode() + b"\0"
        if vm_bytes(data, segments, int(row["name_address"], 16), len(encoded)) != encoded:
            raise ValueError(f"manufacturer label differs: {name}")
        vendor, product = struct.unpack("<II", vm_bytes(data, segments, int(row["pair_address"], 16), 8))
        usb_id = f"{vendor:04x}:{product:04x}"
        if usb_id != row["usb_id"]:
            raise ValueError(f"manufacturer identity differs: {name}")
        rows.append({"manufacturer_name": name, "usb_id": usb_id, "model_id": row["model_id"]})
    return rows


# Parse options and run the vsd inventory workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="VSD Craft.app directory; never executed")
    parser.add_argument("--evidence", type=Path, default=Path(__file__).resolve().parents[1] / "hardware/retail-protocol-sources.v1.json")
    args = parser.parse_args()
    evidence = json.loads(args.evidence.read_text())
    print(json.dumps(inspect(args.app, evidence), indent=4))


if __name__ == "__main__":
    main()
