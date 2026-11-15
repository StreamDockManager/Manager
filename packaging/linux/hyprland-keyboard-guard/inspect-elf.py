"""Read ELF symbol tables; never dlopen or execute the module/compositor."""
from pathlib import Path
import re
import subprocess
import sys

module = Path(sys.argv[1])
out = Path(sys.argv[2])

# Run the selected command and collect its result
def command(*args):
    return subprocess.check_output(args, text=True)

# Collect defined symbols from the inspected ELF binary
def defined(path):
    return {
        line.split()[-1].split("@")[0]
        for line in command("nm", "-D", "--defined-only", str(path)).splitlines()
        if line.split()
    }

needed = re.findall(r"\(NEEDED\).*?\[(.*?)\]", command("readelf", "-d", str(module)))
providers = [Path("/usr/bin/Hyprland"), *[Path("/usr/lib") / name for name in needed]]
available = set()
for provider in providers:
    available.update(defined(provider))
undefined = {
    fields[-1].split("@")[0]
    for line in command("nm", "-D", "--undefined-only", str(module)).splitlines()
    if (fields := line.split()) and fields[0] == "U"
}
unmatched = sorted(undefined - available)
(out / "unmatched-strong-symbols.txt").write_text("\n".join(unmatched) + ("\n" if unmatched else ""))
(out / "symbol-provider-check.txt").write_text(
    f"Static symbol-name check only; no module or compositor loaded.\n"
    f"Strong undefined symbols: {len(undefined)}\nUnmatched: {len(unmatched)}\n"
    + "Providers:\n" + "\n".join(map(str, providers)) + "\n"
)
versions = re.findall(r"(?:GLIBCXX|GLIBC|CXXABI)_[0-9.]+", command("readelf", "--version-info", str(module)))
with (out / "symbol-version-minimums.txt").open("w") as file:
    for prefix in ("GLIBC", "GLIBCXX", "CXXABI"):
        values = {v for v in versions if v.startswith(prefix + "_")}
        maximum = max(values, key=lambda v: tuple(map(int, v.split("_")[1].split("."))), default="none")
        file.write(f"{prefix}: {maximum}\n")
if unmatched:
    print("Unmatched strong symbols:", *unmatched, sep="\n", file=sys.stderr)
    sys.exit(1)
