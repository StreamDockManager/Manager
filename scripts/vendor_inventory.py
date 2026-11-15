#!/usr/bin/env python3
"""Inspect a pinned SDK checkout without importing or executing vendor code.

Run with the checkout and an output path. Output is evidence for review, never a
device descriptor generator: cross-language disagreements require a decision.
"""
import argparse
import ast
import json
import re
import subprocess
from pathlib import Path


# Extract pinned vendor product registrations without importing vendor code
def inventory(root):
    product_path = "Python-SDK/src/StreamDock/ProductIDs.py"
    tree = ast.parse((root / product_path).read_text())
    constants = {}
    registrations = []
    for node in tree.body:
        if isinstance(node, ast.ClassDef):
            for field in node.body:
                if isinstance(field, ast.Assign) and isinstance(field.value, ast.Constant):
                    constants[(node.name, field.targets[0].id)] = field.value.value
        if isinstance(node, ast.Assign) and any(
            isinstance(t, ast.Name) and t.id == "g_products" for t in node.targets
        ):
            for row in node.value.elts:
                vendor, product, model = row.elts
                registrations.append({
                    "usb_id": "%04x:%04x" % (
                        (vendor.value if isinstance(vendor, ast.Constant) else constants[(vendor.value.id, vendor.attr)]),
                        (product.value if isinstance(product, ast.Constant) else constants[(product.value.id, product.attr)]),
                    ),
                    "vendor_model": model.id,
                    "source": product_path,
                    "line": row.lineno,
                })
    cpp = []
    for path in sorted((root / "CPP-SDK/src/HotspotDevice").rglob("*.cpp")):
        source = re.sub(r"/\*.*?\*/|//[^\n]*", "", path.read_text(), flags=re.S)
        values = {k: int(v, 16) for k, v in re.findall(r"(\w+)\s*=\s*(0x[\da-fA-F]+)", source)}
        for match in re.finditer(r"registerDevice\(\s*(\w+)\s*,\s*(\w+)", source):
            a, b = match.groups()
            cpp.append({"usb_id": "%04x:%04x" % (values[a], values[b]),
                        "vendor_model": path.parent.name,
                        "source": str(path.relative_to(root))})
    return {"schema_version": 1, "python_registrations": registrations,
            "cpp_registrations": cpp,
            "python_only": sorted({r["usb_id"] for r in registrations} - {r["usb_id"] for r in cpp}),
            "cpp_only": sorted({r["usb_id"] for r in cpp} - {r["usb_id"] for r in registrations})}


# Parse options and run the vendor inventory workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checkout", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = inventory(args.checkout)
    result["revision"] = subprocess.check_output(
        ["git", "-C", str(args.checkout), "rev-parse", "HEAD"], text=True
    ).strip()
    args.output.write_text(json.dumps(result, indent=4) + "\n")
    print(f"{len(result['python_registrations'])} Python registrations; "
          f"{len(result['cpp_registrations'])} C++ registrations; "
          f"C++ only: {result['cpp_only']}")


if __name__ == "__main__":
    main()
