# Contributing

Re:Earth Buildings is a small Cloudflare Workers + Rust→WASM project that turns Overture Maps building footprints into 3D Tiles on demand. This document covers everything a contributor needs to get oriented; for **dataset usage** (URLs, formats, attribution requirements) see [`README.md`](./README.md).

## Tech stack

| Layer | Tech |
|---|---|
| Runtime | Cloudflare Workers (`workerd`) |
| Hot path | **Rust → WASM** via `wasm-pack --target web` |
| HTTP routing | [Hono](https://hono.dev/) |
| Storage | Cloudflare R2 (cache only — no source data) |
| Metadata KV | Workers KV (latest Overture release pointer) |
| Edge cache | Cloudflare Cache API |
| Build | `wasm-pack`, `wrangler`, `pnpm` |

**Why Workers:** zero-egress R2, free 300+ PoP edge cache, autoscale, no servers to babysit. The 30 s CPU / 128 MB memory ceiling is plenty for tile-scoped work.

## Design principles

1. **Zero batch jobs.** No pre-tiling, no scheduled syncs. Every output tile is generated on demand and cached.
2. **Deterministic per tile.** Same input → same output. Maximises cache hit rate.
3. **PMTiles read directly from upstream.** We range-read Overture's public bucket — no self-hosted mirror.
4. **Terrain read directly from Re:Earth Terrain.** Same idea: per-request fetch of `terrain.reearth.land/terrarium/ellipsoid/{z}/{x}/{y}.webp`, decoded inside WASM.
5. **R2 holds only the rendered cache.** No source data lives in our buckets.
6. **Worker is thin; compute lives in WASM.** TypeScript orchestrates I/O, Rust does the geometry.
7. **Honest 503s on timeout.** If we hit the 30 s wall, surface it; don't hide it behind a half-broken response.

## Repository layout

```
.
├── apps/worker/                Cloudflare Worker (TypeScript host)
│   ├── src/index.ts            Hono app + CORS
│   ├── src/routes/             tileset.json / glb / viewer
│   ├── src/pmtiles.ts          Overture PMTiles range-read client
│   ├── src/terrain.ts          Re:Earth Terrain WebP fetcher
│   ├── src/version.ts          Overture release auto-discovery
│   ├── src/lod.ts              LOD policy (zoom buckets, area filters, simplify)
│   ├── src/wasm.ts             wasm-bindgen wrapper
│   └── wrangler.toml
├── crates/
│   ├── mvt-decoder/            Minimal MVT decoder (Overture buildings layer)
│   ├── terrain-decoder/        Mapzen Terrarium WebP decoder + bilinear sampler
│   ├── buildings-core/         Mesh + glb writer
│   └── buildings-wasm/         wasm-bindgen entry point
└── README.md                   For dataset users
```

## Local development

Prereqs: Rust (stable), `wasm-pack`, `wasm-opt` (Homebrew: `brew install binaryen`), Node 20+, `pnpm`, `wrangler`.

```bash
cd apps/worker
pnpm install

# Build the WASM blob (also chained from `pnpm run dev` / `deploy`)
pnpm run build:wasm

# Local dev server (binds R2 + KV locally)
pnpm run dev    # → http://localhost:8788
```

Useful URLs once `wrangler dev` is up:

| URL | Purpose |
|---|---|
| `http://localhost:8788/` | Bundled Cesium viewer (terrain, sun, atmosphere ON) |
| `http://localhost:8788/tileset.json` | Root 3D Tiles entry point |
| `http://localhost:8788/v3-add/14/14552/6451.glb` | A specific glb tile (Tokyo Station area) |

`.dev.vars` can set `CACHE_DISABLED=1` to bypass every cache layer (Cache API + R2 + browser) — useful when iterating on the renderer.

### Other commands

```bash
pnpm run typecheck        # wrangler types + tsc --noEmit
pnpm run lint             # biome check
cargo test --workspace    # Rust unit tests
cargo check --workspace   # Fast type check
```

## Request flow

```
GET /tileset.json
  └─ unversioned root → external child /:impl/sub/0/0/0/tileset.json
       └─ recurse through navigation quadtree until LEAF_PARENT_Z (=11)
            └─ inline content subtree z=12..14 with refine: ADD
                 └─ leaf tiles → /:impl/{z}/{x}/{y}.glb

GET /:impl/{z}/{x}/{y}.glb
  ├─ resolve current Overture release (KV-cached, ~24h)
  ├─ fetch source MVTs at z=14 covering the output tile (1, 4, or 16 tiles)
  ├─ fetch terrain WebP at the same (z, x, y)
  ├─ hash everything → ETag + R2 cache key
  ├─ R2 cache HIT? → stream out
  └─ MISS → WASM pipeline:
        1. decode each MVT (`building` layer only) — extract footprints + Overture attrs
        2. decode terrain WebP (Mapzen Terrarium → ellipsoidal metres)
        3. group footprints by feature_id; compute area-weighted centroid
        4. per building: resolve height (5-step cascade), sample terrain at centroid
        5. extrude polygons; write glb (EXT_meshopt_compression + structural metadata)
     → write to R2 in waitUntil; respond
```

See `apps/worker/src/routes/glb.ts` and `crates/buildings-core/src/lib.rs`.

## LOD policy

Three content zooms with **size-based bucketing under refine: ADD** — every building is rendered at exactly one zoom level, layers compose without overlap:

| zoom | rendered | footprint area |
|---|---|---|
| 12 | mega landmarks | ≥ 10 000 m² (block-mesh / AABB collapse) |
| 13 | large buildings | 2 000 – 10 000 m² |
| 14 | everything smaller | < 2 000 m² (full extrusion) |

Toggle via `LOD_MODE` (`apps/worker/src/lod.ts`); flipping ADD ↔ REPLACE bumps the URL space (it's part of `IMPL_VERSION`).

## Height resolution cascade

ML-derived footprints (Microsoft / Google open buildings) make up most of the world but typically have no metadata. We resolve a usable height per building via a 5-step cascade:

1. **`explicit`** — Overture `height` (best — usually OSM-tagged)
2. **`num_floors`** — `num_floors × 3 m`
3. **`class`** — class lookup table (~50 values, see `mesh.rs:class_default_height`)
4. **`subtype`** — coarser 13-value subtype lookup
5. **`footprint`** / **`density`** — area heuristic. For urban / dense-urban tiles (classified by source-tile building count) a minimum height kicks in (9 m / 12 m) so the small "pencil buildings" of central Tokyo don't render as huts. Method tag is `density` when this boost applied.

The glb exposes `height` (used for extrusion), `source_height` (raw Overture value, 0 = absent), and `height_method` so styling code can distinguish trusted heights from guesses.

## Caching

Three layers, in order:

1. **Cache API** — Cloudflare's free per-PoP cache, ~ms hit latency, keyed by URL.
2. **R2** — persistent backstop, key = `cache/buildings/{IMPL_VERSION}/{contentHash}.glb`.
3. **Regenerate** — only when both miss.

The glb route also serves browser ETags so clients revalidate cheaply across pmtiles releases that don't actually change the underlying tile bytes.

### `IMPL_VERSION` (URL prefix)

Bump `RENDERER_VERSION` in `apps/worker/src/version.ts` when the renderer or glb schema changes. The bump invalidates every cached URL atomically (versioned URLs change). Daily-ish Overture release rotations are *not* propagated through `IMPL_VERSION` — instead we hash the contributing MVT bytes for the ETag / R2 cache key, so unchanged tiles stay deduped across releases.

## Workers constraints to keep in mind

| Limit | Mitigation |
|---|---|
| 30 s CPU per request | Keep work tile-scoped; use `executionCtx.waitUntil` for cache writes |
| 128 MB memory | Stream MVTs / WebP through WASM; don't materialise multiple copies |
| 10 MB compressed bundle | Track the `.wasm` size; `wasm-opt -O4` is mandatory (built into `wasm-pack`) |
| 50 sub-requests per request | A z=12 output reads 16 z=14 MVTs + 1 terrain — well within budget |

## Deploy

```bash
pnpm run deploy:staging   # → staging-buildings.reearth.land
pnpm run deploy:prod      # → buildings.reearth.land
```

Both auto-discover the latest Overture release on cold start (KV-cached for 24 h). Pin a release manually by writing `overture:latest` in the `PMTILES_DIR` KV namespace.

R2 bucket and KV namespace IDs in `wrangler.toml` are placeholders for the dev environment — substitute real IDs before running locally with persistent state, or just rely on Wrangler's local R2 / KV simulation (`pnpm run dev` does this by default).

## Open questions / nice-to-haves

- **`building_part` layer** — currently skipped. Composite buildings (Skytree, station roofs) lose detail. Would extend `mvt-decoder` to read both layers and composite by `building_id`.
- **Density classifier** — currently uses a fixed threshold table calibrated against Japan. International coverage may need adjustment.
- **Roof shape geometry** — Overture has rich `roof_shape` / `roof_height` / `roof_orientation`; we only surface them as metadata, not as actual gabled / hipped meshes.
- **Per-vertex terrain sampling** — currently centroid-only. Buildings on slopes (mountainside cabins) sit on a single elevation; sampling per vertex would conform but cost more.
- **Cesium ion compatibility** — investigate publishing the tileset under an ion catalog so Cesium-built apps can swap in `Cesium.IonResource` directly.

## Pull requests

- Run `cargo check --workspace`, `pnpm run typecheck`, `pnpm run lint` before pushing.
- Bump `RENDERER_VERSION` if the glb schema or geometry semantics change (otherwise edge caches will serve stale bytes against the new code).
- Keep `README.md` accurate when changing data sources, attribution requirements, or the glb property table — it's the user-facing contract.
