# Re:Earth Buildings

An open, on-demand 3D buildings tile service. **3D Tiles 1.1 + glTF**, generated at the edge from Overture Maps Foundation footprints with ground elevation from [Re:Earth Terrain](https://terrain.reearth.land/). Designed as a drop-in alternative to Cesium ion's OSM Buildings.

> Companion service: **Re:Earth Terrain** at [`terrain.reearth.land`](https://terrain.reearth.land/).

## Live endpoints

| | Production | Staging |
|---|---|---|
| Root tileset | `https://buildings.reearth.land/tileset.json` | `https://staging-buildings.reearth.land/tileset.json` |
| Bundled viewer | `https://buildings.reearth.land/` | `https://staging-buildings.reearth.land/` |

CORS is wide open (`Access-Control-Allow-Origin: *`); fetch directly from the browser.

## Quick start

### Cesium

```js
import * as Cesium from "cesium";

const viewer = new Cesium.Viewer("app");

const buildings = await Cesium.Cesium3DTileset.fromUrl(
  "https://buildings.reearth.land/tileset.json",
);
viewer.scene.primitives.add(buildings);

// Recommended companion: Re:Earth Terrain so the ground meets the building bases.
viewer.scene.setTerrain(
  new Cesium.Terrain(
    Cesium.CesiumTerrainProvider.fromUrl(
      "https://terrain.reearth.land/cesium-mesh/ellipsoid",
      { requestVertexNormals: true },
    ),
  ),
);
```

### three.js / loaders.gl

Any 3D Tiles 1.1 client with `EXT_meshopt_compression` and `EXT_structural_metadata` support can load the tileset directly. With `loaders.gl`:

```js
import { load } from "@loaders.gl/core";
import { Tiles3DLoader } from "@loaders.gl/3d-tiles";

const tileset = await load(
  "https://buildings.reearth.land/tileset.json",
  Tiles3DLoader,
);
```

## Format

- **3D Tiles 1.1** with `refine: ADD` (size-bucketed LOD: mega landmarks at z=12, large at z=13, everything else at z=14)
- **glTF 2.0 binary** (`.glb`) per content tile
- Required extensions: `EXT_meshopt_compression`, `EXT_mesh_features`, `EXT_structural_metadata`
- WGS84 ellipsoid-anchored — per-building ground elevation is **already baked into the geometry** using Re:Earth Terrain (ellipsoid mode, EGM2008 blended in upstream). Use it with any terrain provider that exposes ellipsoidal heights, or with a flat ellipsoid; Cesium World Terrain users may see vertical mismatches.

### Per-building metadata (EXT_structural_metadata)

| Property | Type | Notes |
|---|---|---|
| `feature_id` | UINT64 | Planetiler-hashed Overture id, stable within a release |
| `gers_id` | STRING | GERS feature id, stable across releases |
| `name` | STRING | Primary name from Overture `names.primary` |
| `subtype` | STRING | Overture `subtype` enum (`residential`, `commercial`, …) |
| `class` | STRING | Overture `class` enum (`apartments`, `house`, `industrial`, …) |
| `height` | FLOAT32 | Height in metres used for the extrusion (always populated) |
| `source_height` | FLOAT32 | Original Overture `height`, or `0` when absent |
| `height_method` | STRING | `explicit` / `num_floors` / `class` / `subtype` / `footprint` / `density` (see below) |
| `min_height` | FLOAT32 | For partial-height buildings (pilotis, overhangs) |
| `roof_height` | FLOAT32 | From Overture (0 = absent) |
| `ground_elev` | FLOAT32 | Ellipsoidal ground elevation sampled at the centroid |
| `num_floors` | UINT16 | From Overture (0 = absent) |
| `roof_shape` | STRING | Overture enum (`flat`, `gabled`, `hipped`, …) |

#### How `height` is decided

Overture publishes an explicit `height` for the OSM-tagged minority of buildings; the ML-derived footprints (Microsoft / Google open buildings) typically carry no height at all. We resolve a usable height per building via a 5-step cascade — `height_method` tells you which one fired:

1. `explicit` — Overture `height`
2. `num_floors` — `num_floors × 3 m`
3. `class` — Overture `class` lookup table (~50 values: `apartments` → 25 m, `house` → 6 m, `industrial` → 10 m, …)
4. `subtype` — coarser 13-value `subtype` lookup (`commercial` → 15 m, `residential` → 8 m, …)
5. `footprint` / `density` — area heuristic, with a per-tile density boost (urban → min 9 m / 3 floors; dense urban → 12 m+) so central-Tokyo "pencil buildings" don't render as huts. Tag is `density` when the boost applied, `footprint` otherwise

If you need stricter visuals, filter on `height_method` (e.g. show only `explicit` / `num_floors`) or restyle by tag.

## Source data

| Layer | Source | License |
|---|---|---|
| Building footprints + attributes | [Overture Maps Foundation](https://overturemaps.org/) `buildings` theme — monthly releases | **ODbL 1.0** |
| Ground elevation | [Re:Earth Terrain](https://terrain.reearth.land/) (Mapterhorn DEM blended with EGM2008) | See terrain service |

Overture's buildings theme conflates ~2.6 B footprints from OSM, Microsoft Building Footprints, Google Open Buildings, Esri Community Maps, IGN España, and others under a normalised attribute schema. We read the public PMTiles distribution directly via HTTP Range — no self-hosted mirror.

## License & required attribution

The **rendered 3D Tiles output** (`.glb` bytes you fetch from this service) is a "Produced Work" of an OSM/Overture-derived database. ODbL 1.0 does **not** trigger Share-Alike on Produced Works, but **attribution is required** when displaying the output.

### Minimum attribution string

```
Buildings © OpenStreetMap contributors, Overture Maps Foundation (ODbL).
Terrain © Re:Earth Terrain (Mapterhorn / EGM2008).
```

The tileset already declares this in `asset.copyright` of `tileset.json`; Cesium and most 3D Tiles loaders surface it automatically. If your client doesn't, render the string yourself in your map credits / about screen.

For the **ODbL-licensed source data** itself (the Overture parquet / PMTiles), the standard ODbL terms apply if you redistribute it. See the [Overture attribution guide](https://docs.overturemaps.org/attribution/).

The **service code in this repository** is licensed separately (see `LICENSE`); using the code to run your own tile service does not change the upstream data licence.

## Versioning

Tile URLs are content-addressable (`/{IMPL_VERSION}/{z}/{x}/{y}.glb`). When the renderer or glb schema changes, `IMPL_VERSION` bumps and clients pick up the new prefix on their next `tileset.json` revalidation. Monthly Overture release rotations are *not* breaking — same URL, same ETag if the underlying tile bytes didn't change.

`/tileset.json` is served with `Cache-Control: max-age=60, must-revalidate` so clients pick up `IMPL_VERSION` bumps within a minute. Sub-tilesets and individual `.glb` URLs are immutable for their content hash.

## Acknowledgments

This project would not exist without the work and ideas of others in the open geospatial community.

- **[OSMBuildings](https://github.com/OSMBuildings/OSMBuildings)** pioneered rendering OpenStreetMap buildings in 3D on the open web. We approach the problem from a different angle — server-side tiling for the 3D Tiles ecosystem rather than a client-side WebGL library — but their work shaped the space we are building in. We also share their concern about open geospatial efforts being absorbed into closed commercial offerings; Re:Earth exists to keep these foundations open.
- **[Overture Maps Foundation](https://overturemaps.org/)** for the buildings PMTiles distribution and the normalised attribute schema.
- **[Protomaps](https://protomaps.com/)** for the PMTiles format and tooling that the Overture tiles ride on.
- **[OpenStreetMap](https://www.openstreetmap.org/)** contributors, whose data is the foundation of all of this.
- **[Cesium](https://cesium.com/)** and the **OGC 3D Tiles** community for the open specification we target.
- **Mapterhorn / NGA EGM2008** for the global elevation data feeding Re:Earth Terrain.

## Contributing

For local development setup, repo layout, request flow, and the geometry / cache internals see [`CONTRIBUTING.md`](./CONTRIBUTING.md).
