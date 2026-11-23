# Offline Natural Earth land outlines

Made with [Natural Earth](https://www.naturalearthdata.com/). The
[1:110m land polygons](https://www.naturalearthdata.com/downloads/110m-physical-vectors/110m-land/)
are [public domain](https://www.naturalearthdata.com/about/terms-of-use/).
Manager bundles these simplified land outlines for the Vault location viewer. The map renders without a network request and is not suitable for navigation.

## Regenerate the map

From the Manager repository root, run the generator with a local copy of
[`ne_110m_land.geojson` from Natural Earth Vector v5.1.2](https://github.com/nvkelso/natural-earth-vector/blob/v5.1.2/geojson/ne_110m_land.geojson).

```sh
python3 scripts/generate-offline-land.py /path/to/ne_110m_land.geojson
```

The generator verifies the source SHA-256, projects WGS84 longitude and latitude
linearly into the SVG viewport, and simplifies the rings at a 0.2-unit tolerance.
