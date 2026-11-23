"""Regenerate Manager's simplified offline land outlines from pinned public Natural Earth GeoJSON"""

import argparse
import hashlib
import json
from pathlib import Path

SOURCE_SHA256 = "9e0729ee253ca7d7a5c4ae9395fb1902264c5377c52e224d13dd85010e2835d9"
WIDTH = 480
HEIGHT = 228
TOLERANCE = 0.20


def distance_squared(point, start, end):
    """Return the squared distance to the segment joining start and end"""
    dx, dy = end[0] - start[0], end[1] - start[1]
    length_squared = dx * dx + dy * dy
    dot = (point[0] - start[0]) * dx + (point[1] - start[1]) * dy
    t = max(0, min(1, dot / length_squared)) if length_squared else 0
    return (point[0] - (start[0] + t * dx)) ** 2 + (point[1] - (start[1] + t * dy)) ** 2


def simplify(points):
    """Thin points within a fixed tolerance while preserving connected coastline"""
    retained = {0, len(points) - 1}
    spans = [(0, len(points) - 1)]
    while spans:
        first, last = spans.pop()
        if last - first < 2:
            continue
        pivot = max(
            range(first + 1, last),
            key=lambda index: distance_squared(points[index], points[first], points[last]),
        )
        if distance_squared(points[pivot], points[first], points[last]) > TOLERANCE**2:
            retained.add(pivot)
            spans.extend(((first, pivot), (pivot, last)))
    return [points[index] for index in sorted(retained)]


def svg_path(document):
    """Convert the pinned WGS84 polygon rings to SVG path segments"""
    rings = []
    for feature in document["features"]:
        geometry = feature["geometry"]
        if geometry["type"] != "Polygon":
            raise ValueError("Expected Natural Earth land polygons")
        for ring in geometry["coordinates"]:
            projected = [
                ((lon + 180) * WIDTH / 360, (90 - lat) * HEIGHT / 180)
                for lon, lat in ring
            ]
            points = [(round(x, 1), round(y, 1)) for x, y in simplify(projected)]
            if len(set(points)) < 3:
                continue
            payload = " ".join(f"{x:g},{y:g}" for x, y in points[:-1])
            rings.append(f"M{payload}Z")
    return "".join(rings)


def main():
    """Validate the input hash before rebuilding the deterministic bundled asset"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="Pinned Natural Earth ne_110m_land.geojson")
    args = parser.parse_args()
    data = args.source.read_bytes()
    if hashlib.sha256(data).hexdigest() != SOURCE_SHA256:
        parser.error("Natural Earth source SHA-256 does not match the pinned input")
    output = Path(__file__).resolve().parents[1] / "src" / "map-data" / "natural-earth-land.v5.1.2.json"
    asset = {"width": WIDTH, "height": HEIGHT, "path": svg_path(json.loads(data))}
    output.write_text(json.dumps(asset, indent=4) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
