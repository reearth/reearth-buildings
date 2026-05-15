# Re:Earth Buildings

An on-demand 3D buildings tile server built on Cloudflare Workers, designed as an open alternative to Cesium ion's OSM Buildings.

Re:Earth Buildings turns OpenStreetMap building footprints into standards-compliant **3D Tiles 1.1 + glTF** tiles, generated at the edge in Cloudflare Workers (Rust → WASM). It is intended as a drop-in source for Cesium, MapLibre, three.js, and any other client that speaks 3D Tiles.

Companion project: **[Re:Earth Terrain](https://terrain.reearth.land)** — open terrain tiles for the same ecosystem.

## Architecture in one paragraph

The [Overture Maps Foundation](https://overturemaps.org/) buildings PMTiles is read directly via HTTP Range from Cloudflare Workers. For each output tile, the Worker also fetches the matching [Re:Earth Terrain](https://terrain.reearth.land/) Terrarium WebP and a Rust core (compiled to WASM) decodes both, samples the ground elevation per building, and extrudes footprints into glb meshes anchored on the WGS84 ellipsoid. Output is cached in the Cloudflare Cache API and R2. No batch pipelines, no pre-tiling — every tile is generated on demand and cached.

See [`cloudflare-architecture.md`](./cloudflare-architecture.md) for the full design.

## Acknowledgments

This project would not exist without the work and ideas of others in the open geospatial community.

- **[OSMBuildings](https://github.com/OSMBuildings/OSMBuildings)** pioneered rendering OpenStreetMap buildings in 3D on the open web. We approach the problem from a different angle — server-side tiling for the 3D Tiles ecosystem rather than a client-side WebGL library — but their work shaped the space we are building in. We also share their concern about open geospatial efforts being absorbed into closed commercial offerings; Re:Earth exists to keep these foundations open.
- **[Overture Maps Foundation](https://overturemaps.org/)** for the buildings PMTiles distribution that makes on-demand tile generation feasible at the edge, and for normalising attributes (height, num_floors, roof_shape, …) across OSM, ML-derived, and authoritative sources.
- **[Protomaps](https://protomaps.com/)** for the PMTiles format and tooling that the Overture tiles ride on.
- **[OpenStreetMap](https://www.openstreetmap.org/)** contributors, whose data is the foundation of all of this.
- **[Cesium](https://cesium.com/)** and the **OGC 3D Tiles** community for the open specification we target.

## License

See `LICENSE` (TBD).
