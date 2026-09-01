//! Extrude Overture MVT building polygons (possibly drawn from several
//! source tiles) into a flat-shaded triangle mesh anchored at an output
//! tile.
//!
//! Output frame: tile-local ENU metres (east/north/up), centred on the
//! output tile's centre. y is up in the glb-friendly remapping below.
//! Per-building ground elevation comes from a Re:Earth Terrain tile sampled
//! at each building's centroid.

use crate::coord::{self, LonLat};
use crate::height_config::{HeightConfig, ModelMode};
use crate::{features, height_model};
use mvt_decoder::{BuildingFeature, DecodedTile};
use std::collections::HashMap;
use terrain_decoder::TerrainTile;

/// Per-building metadata, indexed by FEATURE_ID written into the mesh.
#[derive(Debug, Clone, Default)]
pub struct FeatureProps {
    /// MVT feature id (Planetiler-hashed Overture id). Stable for dedup
    /// across source tiles within one PMTiles release.
    pub feature_id: Option<u64>,
    /// GERS feature id (string). Stable across releases.
    pub gers_id: Option<String>,
    pub name: Option<String>,
    pub subtype: Option<String>,
    pub class: Option<String>,
    /// Resolved height used for the extrusion. Always positive.
    pub height_m: f32,
    /// Original Overture `height` value, if present. `None` when the
    /// upstream property was missing or 0; surfaces in glb as 0 with
    /// `noData=0`.
    pub source_height_m: Option<f32>,
    /// Tag identifying which cascade path produced [`height_m`]. One of
    /// `explicit` / `num_floors` / `model` / `class` / `subtype` /
    /// `footprint` / `density`. `model` fires only when the config's
    /// [`ModelMode`](crate::height_config::ModelMode) is `On`; the trailing
    /// four are the legacy `Off` fallbacks.
    pub height_method: &'static str,
    pub min_height_m: f32,
    pub roof_height_m: f32,
    pub roof_shape: Option<String>,
    pub num_floors: u16,
    /// Ground elevation (ellipsoidal metres) sampled at the centroid.
    pub ground_elev_m: f32,
    pub footprint_m2: f32,
}

pub struct Mesh {
    pub positions: Vec<f32>,
    pub normals: Vec<f32>,
    pub feature_ids: Vec<u16>,
    pub indices: Vec<u32>,
    pub features: Vec<FeatureProps>,
}

pub struct Source<'a> {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub tile: &'a DecodedTile,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AreaFilter {
    pub min_m2: f32,
    pub max_m2: f32,
}

impl AreaFilter {
    pub fn accepts(&self, area_m2: f32) -> bool {
        if self.min_m2 > 0.0 && area_m2 < self.min_m2 {
            return false;
        }
        if self.max_m2 > 0.0 && area_m2 >= self.max_m2 {
            return false;
        }
        true
    }
}

/// Coarse "how built-up is this neighbourhood" classifier derived from
/// the source-tile building count. Drives a minimum height for buildings
/// that fall through to the footprint heuristic — the small "pencil
/// buildings" of central Tokyo would otherwise look like sheds.
///
/// Only applied to `footprint`-method heights; explicit / num_floors /
/// class / subtype values are trusted as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrbanLevel {
    Rural,
    Suburban,
    Urban,
    DenseUrban,
}

impl UrbanLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            UrbanLevel::Rural => "rural",
            UrbanLevel::Suburban => "suburban",
            UrbanLevel::Urban => "urban",
            UrbanLevel::DenseUrban => "dense_urban",
        }
    }
}

/// Classify by average building count per source MVT (z=14). Thresholds
/// are tuned against Japanese urban patterns; rural Hokkaido sits well
/// below 200, Tokyo 23-ku averages 800–2000, Marunouchi-class blocks
/// push past 1500.
pub fn classify_urban(avg_buildings_per_source: f32, cfg: &HeightConfig) -> UrbanLevel {
    let t = &cfg.urban_thresholds;
    if avg_buildings_per_source >= t.dense_urban_min {
        UrbanLevel::DenseUrban
    } else if avg_buildings_per_source >= t.urban_min {
        UrbanLevel::Urban
    } else if avg_buildings_per_source >= t.suburban_min {
        UrbanLevel::Suburban
    } else {
        UrbanLevel::Rural
    }
}

/// Everything the height cascade needs, decoupled from `BuildingFeature`
/// so both the renderer and the height-optimizer resolve height through
/// the same code path — with *merged* (multi-fragment) geometry and the
/// building's home-tile [`features::TileContext`]. String fields borrow
/// from the caller's owned attributes.
pub struct HeightCascadeInput<'a> {
    /// Overture `height`, already filtered to `> 0`.
    pub explicit_height_m: Option<f64>,
    /// Overture `num_floors`, already filtered to `> 0`.
    pub num_floors: Option<u32>,
    pub class: Option<&'a str>,
    pub subtype: Option<&'a str>,
    /// Merged footprint area across all fragments.
    pub footprint_m2: f32,
    /// Merged outer-ring perimeter across all fragments.
    pub perimeter_m: f32,
    pub has_name: bool,
    pub has_parts: bool,
    pub roof_shape: Option<&'a str>,
    pub min_height_m: Option<f32>,
    pub tile: features::TileContext,
}

/// Pick the building height in metres via the fallback cascade:
///
///   1. explicit Overture `height` (best — usually OSM-tagged)
///   2. `num_floors × meters_per_floor`
///
/// then, when [`cfg.model`](HeightConfig::model) is [`ModelMode::On`]:
///
///   3. the GBT model (`model`)
///
/// or, when it is [`ModelMode::Off`] (the default), the legacy tables:
///
///   3. class lookup (`apartments`, `house`, `industrial`, …)
///   4. subtype lookup (coarser: `residential`, `commercial`, …)
///   5. footprint area heuristic, optionally boosted by neighbourhood
///      density (last resort for ML-derived footprints with no metadata)
///
/// Returns `(height_m, method)` where `method` is a stable tag the glb
/// surfaces as a property so downstream styling can distinguish
/// "trust this" from "we guessed".
pub fn default_height_meters(
    input: &HeightCascadeInput<'_>,
    urban: UrbanLevel,
    cfg: &HeightConfig,
) -> (f64, &'static str) {
    if let Some(h) = input.explicit_height_m {
        if h > 0.0 {
            return (h, "explicit");
        }
    }
    if let Some(l) = input.num_floors {
        if l > 0 {
            return ((l as f64) * cfg.meters_per_floor, "num_floors");
        }
    }
    if cfg.model == ModelMode::On {
        let fi = features::FeatureInput {
            footprint_m2: input.footprint_m2,
            perimeter_m: input.perimeter_m,
            class: input.class,
            subtype: input.subtype,
            has_name: input.has_name,
            has_parts: input.has_parts,
            roof_shape: input.roof_shape,
            min_height_m: input.min_height_m,
            tile: input.tile,
        };
        return (
            height_model::builtin().predict_height_m(&fi) as f64,
            "model",
        );
    }
    if let Some(cls) = input.class {
        if let Some(h) = cfg.class_height_m.get(cls) {
            return (*h, "class");
        }
    }
    if let Some(st) = input.subtype {
        if let Some(h) = cfg.subtype_height_m.get(st) {
            return (*h, "subtype");
        }
    }
    let curve = match urban {
        UrbanLevel::DenseUrban => &cfg.footprint.dense_urban,
        UrbanLevel::Urban => &cfg.footprint.urban,
        UrbanLevel::Suburban => &cfg.footprint.suburban,
        UrbanLevel::Rural => &cfg.footprint.rural,
    };
    let h = curve.lookup(input.footprint_m2);
    let method = if urban == UrbanLevel::Rural {
        "footprint"
    } else {
        // Density-boosted: distinguishable in the glb so users can
        // filter / restyle the guessed-tall buildings.
        "density"
    };
    (h, method)
}

/// Raw per-building attributes the height cascade needs, captured once
/// from the fragment that first creates a pending entry. Held on the
/// pending struct so both [`build_mesh`] and [`extract_buildings`] can
/// resolve height in their emit loops from *merged* area/perimeter and
/// the home tile's context, via the shared [`resolve_pending_height`].
struct HeightInputs {
    explicit_height_m: Option<f64>,
    num_floors: Option<u32>,
    class: Option<String>,
    subtype: Option<String>,
    has_name: bool,
    has_parts: bool,
    roof_shape: Option<String>,
    min_height_m: Option<f32>,
}

/// Capture the cascade-relevant attributes of one raw feature. `height`
/// and `num_floors` are pre-filtered to their positive-only forms so the
/// cascade's steps 1-2 (and the anchor stats in [`features::tile_context`])
/// agree on what counts as "present".
fn height_inputs_from(feat: &BuildingFeature) -> HeightInputs {
    HeightInputs {
        explicit_height_m: feat.height.filter(|h| *h > 0.0),
        num_floors: feat.num_floors.filter(|l| *l > 0),
        class: feat.class.clone(),
        subtype: feat.subtype.clone(),
        has_name: feat.name.as_deref().is_some_and(|n| !n.is_empty()),
        has_parts: feat.has_parts == Some(true),
        roof_shape: feat.roof_shape.clone(),
        min_height_m: feat.min_height.map(|h| h as f32),
    }
}

/// Resolve a pending building's height from its merged geometry and
/// home-tile context. The single shared call site for both emit loops,
/// so the renderer and the optimizer can never skew.
fn resolve_pending_height(
    inputs: &HeightInputs,
    total_area_m2: f32,
    total_perimeter_m: f32,
    ctx: features::TileContext,
    urban: UrbanLevel,
    cfg: &HeightConfig,
) -> (f32, &'static str) {
    let cascade = HeightCascadeInput {
        explicit_height_m: inputs.explicit_height_m,
        num_floors: inputs.num_floors,
        class: inputs.class.as_deref(),
        subtype: inputs.subtype.as_deref(),
        footprint_m2: total_area_m2,
        perimeter_m: total_perimeter_m,
        has_name: inputs.has_name,
        has_parts: inputs.has_parts,
        roof_shape: inputs.roof_shape.as_deref(),
        min_height_m: inputs.min_height_m,
        tile: ctx,
    };
    let (h, method) = default_height_meters(&cascade, urban, cfg);
    (h as f32, method)
}

/// One Overture building's resolved metadata + polygon ring(s) in
/// lon/lat. Produced by [`extract_buildings`]; used by the
/// height-optimizer to evaluate the height cascade against ground
/// truth without re-implementing the de-fragmentation + dedup logic.
#[derive(Debug, Clone)]
pub struct ExtractedBuilding {
    pub feature_id: Option<u64>,
    pub gers_id: Option<String>,
    pub class: Option<String>,
    pub subtype: Option<String>,
    pub footprint_m2: f32,
    /// Merged outer-ring perimeter in metres across all fragments.
    pub perimeter_m: f32,
    pub height_m: f32,
    pub height_method: &'static str,
    pub source_height_m: Option<f32>,
    pub num_floors: Option<u32>,
    pub has_name: bool,
    pub has_parts: bool,
    pub roof_shape: Option<String>,
    /// Overture `min_height`; `None` when the attribute was absent
    /// (distinct from the glb-facing `0.0`).
    pub min_height_m: Option<f32>,
    /// Home-tile surroundings context (the source tile that contributed
    /// this building's first fragment). Feeds the model's anchor stats.
    pub tile: features::TileContext,
    /// Area-weighted centroid (degrees).
    pub centroid: LonLat,
    /// Outer rings of every fragment, in lon/lat. One ring per
    /// fragment — multi-tile buildings have multiple. Used by the
    /// matcher's "point-in-polygon" test.
    pub outer_rings_lonlat: Vec<Vec<LonLat>>,
}

impl ExtractedBuilding {
    /// The exact [`features::FeatureInput`] the wasm inference path sees
    /// for this building. The **single** way the offline trainer may
    /// build a feature vector — hand-assembling one would break the
    /// train/predict parity firewall.
    pub fn feature_input(&self) -> features::FeatureInput<'_> {
        features::FeatureInput {
            footprint_m2: self.footprint_m2,
            perimeter_m: self.perimeter_m,
            class: self.class.as_deref(),
            subtype: self.subtype.as_deref(),
            has_name: self.has_name,
            has_parts: self.has_parts,
            roof_shape: self.roof_shape.as_deref(),
            min_height_m: self.min_height_m,
            tile: self.tile,
        }
    }
}

/// Decode + dedup + height-resolve every building in `sources`.
/// Mirrors [`build_mesh`]'s aggregation pass minus the extrusion. The
/// urban-density classifier runs against the raw building counts so
/// the output matches what `build_mesh` would render for the same
/// input.
pub fn extract_buildings(
    sources: &[Source<'_>],
    height_config: &HeightConfig,
) -> Vec<ExtractedBuilding> {
    let total_buildings: usize = sources.iter().map(|s| s.tile.buildings.len()).sum();
    let avg_per_source = total_buildings as f32 / (sources.len().max(1) as f32);
    let urban = classify_urban(avg_per_source, height_config);

    // ---- Pass A: per-source-tile surroundings context ----
    let contexts: Vec<features::TileContext> = sources
        .iter()
        .map(|s| features::tile_context(s.tile, height_config))
        .collect();

    struct Pending {
        props: FeatureProps,
        height_inputs: HeightInputs,
        /// Source tile (index into `sources`) of this building's first
        /// fragment; selects the [`features::TileContext`] the model sees.
        home_source_idx: usize,
        total_area_m2: f32,
        total_perimeter_m: f32,
        rings: Vec<Vec<LonLat>>,
        centroid_lon_w: f64,
        centroid_lat_w: f64,
        centroid_w: f64,
    }
    let mut by_id: HashMap<u64, usize> = HashMap::new();
    let mut pending: Vec<Pending> = Vec::new();

    // ---- Pass B: collect + dedup fragments ----
    for (src_idx, source) in sources.iter().enumerate() {
        for feat in &source.tile.buildings {
            // Mirror build_mesh: skip explicitly-flagged underground structures
            // so the optimizer evaluates the same set the renderer emits.
            if feat.is_underground_structure() {
                continue;
            }
            let polygons = group_polygons(&feat.rings);
            for polygon in polygons {
                let area = polygon_area_m2(&polygon, source, source.tile.extent) as f32;
                let perimeter = polygon_perimeter_m(&polygon, source, source.tile.extent) as f32;
                let centroid_ll = polygon_centroid_lonlat(&polygon, source, source.tile.extent);
                let outer_ll: Vec<LonLat> = polygon
                    .outer
                    .iter()
                    .map(|p| {
                        coord::tile_xy_to_lonlat(
                            source.z,
                            source.x,
                            source.y,
                            source.tile.extent,
                            p[0],
                            p[1],
                        )
                    })
                    .collect();
                let w = area as f64;
                let make = || Pending {
                    props: feature_props(feat),
                    height_inputs: height_inputs_from(feat),
                    home_source_idx: src_idx,
                    total_area_m2: area,
                    total_perimeter_m: perimeter,
                    rings: Vec::new(),
                    centroid_lon_w: 0.0,
                    centroid_lat_w: 0.0,
                    centroid_w: 0.0,
                };
                let entry_idx = match feat.id {
                    Some(fid) => match by_id.get(&fid).copied() {
                        Some(idx) => {
                            pending[idx].total_area_m2 += area;
                            pending[idx].total_perimeter_m += perimeter;
                            idx
                        }
                        None => {
                            let idx = pending.len();
                            by_id.insert(fid, idx);
                            pending.push(make());
                            idx
                        }
                    },
                    None => {
                        let idx = pending.len();
                        pending.push(make());
                        idx
                    }
                };
                pending[entry_idx].rings.push(outer_ll);
                pending[entry_idx].centroid_lon_w += centroid_ll.lon_deg * w;
                pending[entry_idx].centroid_lat_w += centroid_ll.lat_deg * w;
                pending[entry_idx].centroid_w += w;
            }
        }
    }

    pending
        .into_iter()
        .map(|b| {
            // Resolve height once, from merged geometry and the home
            // tile's context — the shared helper both emit loops use.
            let (h, method) = resolve_pending_height(
                &b.height_inputs,
                b.total_area_m2,
                b.total_perimeter_m,
                contexts[b.home_source_idx],
                urban,
                height_config,
            );
            let centroid = if b.centroid_w > 0.0 {
                LonLat {
                    lon_deg: b.centroid_lon_w / b.centroid_w,
                    lat_deg: b.centroid_lat_w / b.centroid_w,
                }
            } else {
                LonLat {
                    lon_deg: 0.0,
                    lat_deg: 0.0,
                }
            };
            ExtractedBuilding {
                feature_id: b.props.feature_id,
                gers_id: b.props.gers_id,
                class: b.height_inputs.class,
                subtype: b.height_inputs.subtype,
                footprint_m2: b.total_area_m2,
                perimeter_m: b.total_perimeter_m,
                height_m: h,
                height_method: method,
                source_height_m: b.props.source_height_m,
                num_floors: b.height_inputs.num_floors,
                has_name: b.height_inputs.has_name,
                has_parts: b.height_inputs.has_parts,
                roof_shape: b.height_inputs.roof_shape,
                min_height_m: b.height_inputs.min_height_m,
                tile: contexts[b.home_source_idx],
                centroid,
                outer_rings_lonlat: b.rings,
            }
        })
        .collect()
}

/// One clipped polygon belonging to some (possibly multi-fragment) building.
struct Fragment {
    /// Index into the parent `sources` slice. Lets us look up
    /// (src_z, src_x, src_y, extent) later without copying them around.
    source_idx: usize,
    polygon: Polygon,
}

struct PendingFeature {
    props: FeatureProps,
    height_inputs: HeightInputs,
    /// Source tile (index into `sources`) of this building's first
    /// fragment; selects the [`features::TileContext`] the model sees.
    home_source_idx: usize,
    total_area_m2: f32,
    total_perimeter_m: f32,
    fragments: Vec<Fragment>,
    /// Lon/lat sum and weight for the area-weighted centroid (used to
    /// sample terrain). Centroid is computed at emit time so that
    /// multi-fragment buildings get a single ground elevation.
    centroid_lon_weighted: f64,
    centroid_lat_weighted: f64,
    centroid_weight: f64,
}

#[allow(clippy::too_many_arguments)]
pub fn build_mesh(
    out_z: u8,
    out_x: u32,
    out_y: u32,
    sources: &[Source<'_>],
    filter: AreaFilter,
    aabb_only: bool,
    terrain: Option<&TerrainTile>,
    height_config: &HeightConfig,
) -> Mesh {
    let anchor = coord::tile_center(out_z, out_x, out_y);

    // Density classifier from raw building counts (pre-filter).
    // Captures the "this is downtown" signal so the footprint heuristic
    // doesn't render Marunouchi as a row of huts.
    let total_buildings: usize = sources.iter().map(|s| s.tile.buildings.len()).sum();
    let avg_per_source = total_buildings as f32 / (sources.len().max(1) as f32);
    let urban = classify_urban(avg_per_source, height_config);

    // ---- 0. per-source-tile surroundings context (pass A) ----
    let contexts: Vec<features::TileContext> = sources
        .iter()
        .map(|s| features::tile_context(s.tile, height_config))
        .collect();

    // ---- 1. collect all fragments, grouping by feature id ----
    let mut by_id: HashMap<u64, usize> = HashMap::new();
    let mut pending: Vec<PendingFeature> = Vec::new();

    for (src_idx, source) in sources.iter().enumerate() {
        for feat in &source.tile.buildings {
            // Overture flags underground structures (subway malls, underground
            // parking) only rarely — is_underground or a negative level — and
            // ships no usable depth, so we can't bury them. Drop the flagged
            // subset rather than extruding it above ground. Untagged
            // underground (the vast majority) is indistinguishable from real
            // buildings and unavoidably remains.
            if feat.is_underground_structure() {
                continue;
            }
            let polygons = group_polygons(&feat.rings);
            if polygons.is_empty() {
                continue;
            }
            for polygon in polygons {
                let area = polygon_area_m2(&polygon, source, source.tile.extent) as f32;
                // Perimeter measured on the real outer ring, before any
                // aabb collapse, so the feature contract is unaffected by
                // the coarse-LOD silhouette simplification.
                let perimeter = polygon_perimeter_m(&polygon, source, source.tile.extent) as f32;
                let centroid_ll = polygon_centroid_lonlat(&polygon, source, source.tile.extent);
                let polygon = if aabb_only {
                    polygon_to_aabb(&polygon)
                } else {
                    polygon
                };
                let fragment = Fragment {
                    source_idx: src_idx,
                    polygon,
                };
                let w = area as f64;
                let entry_idx = match feat.id {
                    Some(fid) => match by_id.get(&fid).copied() {
                        Some(idx) => {
                            pending[idx].fragments.push(fragment);
                            pending[idx].total_area_m2 += area;
                            pending[idx].total_perimeter_m += perimeter;
                            idx
                        }
                        None => {
                            let idx = pending.len();
                            by_id.insert(fid, idx);
                            pending.push(PendingFeature {
                                props: feature_props(feat),
                                height_inputs: height_inputs_from(feat),
                                home_source_idx: src_idx,
                                total_area_m2: area,
                                total_perimeter_m: perimeter,
                                fragments: vec![fragment],
                                centroid_lon_weighted: 0.0,
                                centroid_lat_weighted: 0.0,
                                centroid_weight: 0.0,
                            });
                            idx
                        }
                    },
                    None => {
                        // No stable id → can't dedup; each unidentified
                        // feature counts as its own building.
                        let idx = pending.len();
                        pending.push(PendingFeature {
                            props: feature_props(feat),
                            height_inputs: height_inputs_from(feat),
                            home_source_idx: src_idx,
                            total_area_m2: area,
                            total_perimeter_m: perimeter,
                            fragments: vec![fragment],
                            centroid_lon_weighted: 0.0,
                            centroid_lat_weighted: 0.0,
                            centroid_weight: 0.0,
                        });
                        idx
                    }
                };
                pending[entry_idx].centroid_lon_weighted += centroid_ll.lon_deg * w;
                pending[entry_idx].centroid_lat_weighted += centroid_ll.lat_deg * w;
                pending[entry_idx].centroid_weight += w;
            }
        }
    }

    // ---- 2. emit features that pass the filter ----
    //
    // Size the geometry buffers up front. Extrusion is deterministic in the
    // ring vertex count N: at most N roof vertices plus 4 per wall quad, so
    // 5N vertices and 9N indices (3 per roof triangle, of which there are at
    // most N-2, plus 6 per quad). Growing from empty instead would leave up
    // to 2x slack in three multi-megabyte buffers plus the copy each
    // reallocation makes — the difference between fitting a dense z=14 tile
    // in the worker's 128 MB isolate and not.
    let ring_verts: usize = pending
        .iter()
        .filter(|b| filter.accepts(b.total_area_m2))
        .flat_map(|b| b.fragments.iter())
        .map(|f| polygon_vertex_count(&f.polygon))
        .sum();
    let vert_ub = ring_verts * 5;
    let mut positions: Vec<f32> = Vec::with_capacity(vert_ub * 3);
    let mut normals: Vec<f32> = Vec::with_capacity(vert_ub * 3);
    let mut feature_ids: Vec<u16> = Vec::with_capacity(vert_ub);
    let mut indices: Vec<u32> = Vec::with_capacity(ring_verts * 9);
    let mut features: Vec<FeatureProps> = Vec::new();

    for mut building in pending {
        if !filter.accepts(building.total_area_m2) {
            continue;
        }
        building.props.footprint_m2 = building.total_area_m2;

        // Resolve height from merged geometry and the home tile's
        // context — the shared helper `extract_buildings` also calls, so
        // the renderer and the optimizer can never disagree.
        let (height_m, height_method) = resolve_pending_height(
            &building.height_inputs,
            building.total_area_m2,
            building.total_perimeter_m,
            contexts[building.home_source_idx],
            urban,
            height_config,
        );
        building.props.height_m = height_m;
        building.props.height_method = height_method;

        // Centroid for terrain sampling — area-weighted so multi-fragment
        // buildings settle on the right ground elevation.
        let ground_elev = if let Some(t) = terrain {
            let (lon, lat) = if building.centroid_weight > 0.0 {
                (
                    building.centroid_lon_weighted / building.centroid_weight,
                    building.centroid_lat_weighted / building.centroid_weight,
                )
            } else {
                (anchor.lon_deg, anchor.lat_deg)
            };
            t.sample(lon, lat)
        } else {
            0.0
        };
        building.props.ground_elev_m = ground_elev;

        let base_h = (ground_elev as f64) + (building.props.min_height_m as f64);
        let top_h = (ground_elev as f64) + (building.props.height_m as f64);
        let fid = features.len().min(u16::MAX as usize - 1) as u16;
        features.push(building.props);

        for fragment in building.fragments {
            let source = &sources[fragment.source_idx];
            extrude_polygon(
                source.z,
                source.x,
                source.y,
                source.tile.extent,
                &fragment.polygon,
                anchor,
                base_h,
                top_h,
                fid,
                &mut positions,
                &mut normals,
                &mut feature_ids,
                &mut indices,
            );
        }
    }
    Mesh {
        positions,
        normals,
        feature_ids,
        indices,
        features,
    }
}

/// Copy a feature's raw glb-facing attributes. Height resolution and
/// `footprint_m2` are deliberately *not* set here — the emit loop fills
/// `height_m` / `height_method` via [`resolve_pending_height`] once the
/// merged geometry is known, and `footprint_m2` once fragments are summed.
fn feature_props(feat: &BuildingFeature) -> FeatureProps {
    let source_height_m = feat.height.filter(|h| *h > 0.0).map(|h| h as f32);
    FeatureProps {
        feature_id: feat.id,
        gers_id: feat.gers_id.clone(),
        name: feat.name.clone(),
        subtype: feat.subtype.clone(),
        class: feat.class.clone(),
        height_m: 0.0,
        source_height_m,
        height_method: "",
        min_height_m: feat.min_height.unwrap_or(0.0) as f32,
        roof_height_m: feat.roof_height.unwrap_or(0.0) as f32,
        roof_shape: feat.roof_shape.clone(),
        num_floors: feat.num_floors.unwrap_or(0).min(u16::MAX as u32) as u16,
        ground_elev_m: 0.0,
        footprint_m2: 0.0,
    }
}

/// One outer ring with zero or more holes. All vertex coords are in tile
/// units (with the closing duplicate vertex stripped).
struct Polygon {
    outer: Vec<[i32; 2]>,
    holes: Vec<Vec<[i32; 2]>>,
}

/// Group MVT rings into polygons (spec §4.3.4.4): tile-space A > 0 starts
/// a new outer; A < 0 is a hole attached to the most recent outer.
fn group_polygons(rings: &[Vec<[i32; 2]>]) -> Vec<Polygon> {
    let mut out: Vec<Polygon> = Vec::new();
    for raw in rings {
        let r = strip_close(raw);
        if r.len() < 3 {
            continue;
        }
        let area = signed_area_tile(&r);
        if area > 0.0 {
            out.push(Polygon {
                outer: r,
                holes: Vec::new(),
            });
        } else if let Some(last) = out.last_mut() {
            last.holes.push(r);
        }
    }
    out
}

/// Ring vertices an extrusion of this polygon will consume — outer plus
/// every hole big enough to survive [`extrude_polygon`]'s `len() < 3` guard.
fn polygon_vertex_count(polygon: &Polygon) -> usize {
    let outer = if polygon.outer.len() >= 3 {
        polygon.outer.len()
    } else {
        0
    };
    outer
        + polygon
            .holes
            .iter()
            .filter(|h| h.len() >= 3)
            .map(Vec::len)
            .sum::<usize>()
}

/// Replace a polygon with its axis-aligned bounding rectangle. The
/// resulting block silhouette has 4 vertices and no holes, which keeps
/// the coarsest LOD level cheap to render (≈12 triangles per building).
fn polygon_to_aabb(polygon: &Polygon) -> Polygon {
    let mut min_x = i32::MAX;
    let mut max_x = i32::MIN;
    let mut min_y = i32::MAX;
    let mut max_y = i32::MIN;
    for p in &polygon.outer {
        if p[0] < min_x {
            min_x = p[0];
        }
        if p[0] > max_x {
            max_x = p[0];
        }
        if p[1] < min_y {
            min_y = p[1];
        }
        if p[1] > max_y {
            max_y = p[1];
        }
    }
    Polygon {
        outer: vec![
            [min_x, min_y],
            [max_x, min_y],
            [max_x, max_y],
            [min_x, max_y],
        ],
        holes: vec![],
    }
}

fn strip_close(ring: &[[i32; 2]]) -> Vec<[i32; 2]> {
    if ring.len() >= 2 && ring.first() == ring.last() {
        ring[..ring.len() - 1].to_vec()
    } else {
        ring.to_vec()
    }
}

fn signed_area_tile(ring: &[[i32; 2]]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let mut a: f64 = 0.0;
    for i in 0..ring.len() {
        let p = ring[i];
        let q = ring[(i + 1) % ring.len()];
        a += (p[0] as f64) * (q[1] as f64) - (q[0] as f64) * (p[1] as f64);
    }
    0.5 * a
}

fn polygon_area_m2(polygon: &Polygon, source: &Source<'_>, extent: u32) -> f64 {
    let src_center = coord::tile_center(source.z, source.x, source.y);
    let outer_area = ring_area_m2(&polygon.outer, source, extent, src_center).abs();
    let hole_area: f64 = polygon
        .holes
        .iter()
        .map(|h| ring_area_m2(h, source, extent, src_center).abs())
        .sum();
    (outer_area - hole_area).max(0.0)
}

fn ring_area_m2(ring: &[[i32; 2]], source: &Source<'_>, extent: u32, anchor: LonLat) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let enu: Vec<[f64; 2]> = ring
        .iter()
        .map(|p| coord::tile_xy_to_enu_at(source.z, source.x, source.y, extent, p[0], p[1], anchor))
        .collect();
    let mut a = 0.0;
    for i in 0..enu.len() {
        let p = enu[i];
        let q = enu[(i + 1) % enu.len()];
        a += p[0] * q[1] - q[0] * p[1];
    }
    0.5 * a
}

/// Outer-ring perimeter in metres (closed-ring edge sum). Sibling of
/// [`polygon_area_m2`]; holes are ignored, matching the feature contract
/// (`FeatureInput::perimeter_m` is outer-ring only). Uses the same
/// source-tile ENU projection as the area, so a fragment's area and
/// perimeter are measured in one consistent metric frame.
fn polygon_perimeter_m(polygon: &Polygon, source: &Source<'_>, extent: u32) -> f64 {
    let src_center = coord::tile_center(source.z, source.x, source.y);
    ring_perimeter_m(&polygon.outer, source, extent, src_center)
}

fn ring_perimeter_m(ring: &[[i32; 2]], source: &Source<'_>, extent: u32, anchor: LonLat) -> f64 {
    if ring.len() < 2 {
        return 0.0;
    }
    let enu: Vec<[f64; 2]> = ring
        .iter()
        .map(|p| coord::tile_xy_to_enu_at(source.z, source.x, source.y, extent, p[0], p[1], anchor))
        .collect();
    // `ring` has its closing duplicate stripped (see `strip_close`), so the
    // wrap-around edge (last → first) is what closes the polygon.
    let mut per = 0.0;
    for i in 0..enu.len() {
        let p = enu[i];
        let q = enu[(i + 1) % enu.len()];
        let dx = q[0] - p[0];
        let dy = q[1] - p[1];
        per += (dx * dx + dy * dy).sqrt();
    }
    per
}

/// Lon/lat centroid of a polygon (outer ring only). Sample-quality
/// centroid for terrain lookup — we don't need geometric accuracy here.
fn polygon_centroid_lonlat(polygon: &Polygon, source: &Source<'_>, extent: u32) -> LonLat {
    let mut sum_lon = 0.0f64;
    let mut sum_lat = 0.0f64;
    let n = polygon.outer.len();
    if n == 0 {
        return coord::tile_center(source.z, source.x, source.y);
    }
    for p in &polygon.outer {
        let ll = coord::tile_xy_to_lonlat(source.z, source.x, source.y, extent, p[0], p[1]);
        sum_lon += ll.lon_deg;
        sum_lat += ll.lat_deg;
    }
    LonLat {
        lon_deg: sum_lon / n as f64,
        lat_deg: sum_lat / n as f64,
    }
}

#[allow(clippy::too_many_arguments)]
fn extrude_polygon(
    src_z: u8,
    src_x: u32,
    src_y: u32,
    extent: u32,
    polygon: &Polygon,
    anchor: LonLat,
    base_height: f64,
    top_height: f64,
    fid: u16,
    positions: &mut Vec<f32>,
    normals: &mut Vec<f32>,
    feature_ids: &mut Vec<u16>,
    indices: &mut Vec<u32>,
) {
    let outer_enu: Vec<[f64; 2]> = polygon
        .outer
        .iter()
        .map(|p| coord::tile_xy_to_enu_at(src_z, src_x, src_y, extent, p[0], p[1], anchor))
        .collect();
    let hole_enus: Vec<Vec<[f64; 2]>> = polygon
        .holes
        .iter()
        .map(|h| {
            h.iter()
                .map(|p| coord::tile_xy_to_enu_at(src_z, src_x, src_y, extent, p[0], p[1], anchor))
                .collect()
        })
        .collect();
    // ----- roof -----
    let mut flat: Vec<f64> = Vec::new();
    for p in &outer_enu {
        flat.push(p[0]);
        flat.push(p[1]);
    }
    let mut hole_indices: Vec<usize> = Vec::with_capacity(hole_enus.len());
    let mut running = outer_enu.len();
    for h in &hole_enus {
        if h.len() < 3 {
            continue;
        }
        hole_indices.push(running);
        for p in h {
            flat.push(p[0]);
            flat.push(p[1]);
        }
        running += h.len();
    }
    let roof_tris = earcutr::earcut(&flat, &hole_indices, 2).unwrap_or_default();

    if !roof_tris.is_empty() {
        let base = positions.len() as u32 / 3;
        let mut all_pts: Vec<[f64; 2]> = Vec::with_capacity(running);
        all_pts.extend_from_slice(&outer_enu);
        for h in &hole_enus {
            all_pts.extend_from_slice(h);
        }
        for p in &all_pts {
            positions.extend_from_slice(&[p[0] as f32, top_height as f32, -p[1] as f32]);
            normals.extend_from_slice(&[0.0, 1.0, 0.0]);
            feature_ids.push(fid);
        }
        for tri in roof_tris.as_chunks::<3>().0 {
            indices.push(base + tri[0] as u32);
            indices.push(base + tri[1] as u32);
            indices.push(base + tri[2] as u32);
        }
    }

    // ----- outer walls -----
    extrude_ring_walls(
        &outer_enu,
        None,
        base_height,
        top_height,
        fid,
        positions,
        normals,
        feature_ids,
        indices,
    );
    for h in &hole_enus {
        extrude_ring_walls(
            h,
            None,
            base_height,
            top_height,
            fid,
            positions,
            normals,
            feature_ids,
            indices,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn extrude_ring_walls(
    enu: &[[f64; 2]],
    skip_edges: Option<&[bool]>,
    base_height: f64,
    top_height: f64,
    fid: u16,
    positions: &mut Vec<f32>,
    normals: &mut Vec<f32>,
    feature_ids: &mut Vec<u16>,
    indices: &mut Vec<u32>,
) {
    if enu.len() < 3 {
        return;
    }
    let mut a_enu = 0.0;
    for i in 0..enu.len() {
        let p = enu[i];
        let q = enu[(i + 1) % enu.len()];
        a_enu += p[0] * q[1] - q[0] * p[1];
    }
    let reverse = a_enu > 0.0;
    let n = enu.len();
    for i in 0..n {
        if let Some(skips) = skip_edges {
            let original_idx = if reverse { (n - 1 - i) % n } else { i };
            if skips[original_idx] {
                continue;
            }
        }
        let ai = if reverse { n - 1 - i } else { i };
        let bi = if reverse {
            (n + n - 2 - i) % n
        } else {
            (i + 1) % n
        };
        let a = enu[ai];
        let b = enu[bi];
        let dx = b[0] - a[0];
        let dz_n = b[1] - a[1];
        let len = (dx * dx + dz_n * dz_n).sqrt().max(1e-9);
        let nx = dz_n / len;
        let nz = -dx / len;
        let base = positions.len() as u32 / 3;
        let verts = [
            [a[0] as f32, base_height as f32, -a[1] as f32],
            [b[0] as f32, base_height as f32, -b[1] as f32],
            [b[0] as f32, top_height as f32, -b[1] as f32],
            [a[0] as f32, top_height as f32, -a[1] as f32],
        ];
        let n_vec = [-nx as f32, 0.0, nz as f32];
        for v in &verts {
            positions.extend_from_slice(v);
            normals.extend_from_slice(&n_vec);
            feature_ids.push(fid);
        }
        indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }
}

/// In-place mesh decimation via meshopt. See lib.rs caller for rationale.
pub fn simplify_mesh(mesh: &mut Mesh, ratio: f32, target_error_m: f32) {
    if ratio >= 1.0 || mesh.indices.len() < 6 {
        return;
    }
    let target = ((mesh.indices.len() as f32 * ratio) as usize).max(3) / 3 * 3;
    let pos_bytes: &[u8] = bytemuck_slice(&mesh.positions);
    let adapter = match meshopt::VertexDataAdapter::new(pos_bytes, 12, 0) {
        Ok(a) => a,
        Err(_) => return,
    };
    let new_indices =
        meshopt::simplify_sloppy(&mesh.indices, &adapter, target, target_error_m, None);
    if !new_indices.is_empty() {
        mesh.indices = new_indices;
    }
}

fn bytemuck_slice(v: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvt_decoder::BuildingFeature;

    /// A ~unit square (CCW in tile space → outer ring) with a usable height.
    fn square(id: u64) -> BuildingFeature {
        BuildingFeature {
            id: Some(id),
            rings: vec![vec![[0, 0], [100, 0], [100, 100], [0, 100], [0, 0]]],
            height: Some(10.0),
            ..Default::default()
        }
    }

    fn build(buildings: Vec<BuildingFeature>) -> Mesh {
        let tile = DecodedTile {
            extent: 4096,
            buildings,
        };
        let sources = [Source {
            z: 14,
            x: 0,
            y: 0,
            tile: &tile,
        }];
        let cfg = HeightConfig::default();
        build_mesh(
            14,
            0,
            0,
            &sources,
            AreaFilter {
                min_m2: 0.0,
                max_m2: 0.0,
            },
            false,
            None,
            &cfg,
        )
    }

    #[test]
    fn drops_flagged_underground_features() {
        let mut ug_flag = square(2);
        ug_flag.is_underground = Some(true);
        let mut ug_level = square(3);
        ug_level.level = Some(-1);

        let mesh = build(vec![square(1), ug_flag, ug_level]);

        let ids: Vec<_> = mesh.features.iter().map(|f| f.feature_id).collect();
        assert_eq!(
            ids,
            vec![Some(1)],
            "only the non-underground building should be emitted"
        );
    }

    #[test]
    fn keeps_above_ground_and_non_negative_level() {
        let mut lvl0 = square(5);
        lvl0.level = Some(0);
        let mut ug_false = square(6);
        ug_false.is_underground = Some(false);

        let mesh = build(vec![square(4), lvl0, ug_false]);

        assert_eq!(
            mesh.features.len(),
            3,
            "level 0, is_underground=false, and unflagged buildings are all kept"
        );
    }

    /// A square ring `side` tile-units wide with its corner at (x0, y0).
    fn ring(x0: i32, y0: i32, side: i32) -> Vec<[i32; 2]> {
        vec![
            [x0, y0],
            [x0 + side, y0],
            [x0 + side, y0 + side],
            [x0, y0 + side],
            [x0, y0],
        ]
    }

    /// A building with no height metadata at all: resolves via the
    /// footprint heuristic (model Off) or the GBT model (On).
    fn bare(id: u64, rings: Vec<Vec<[i32; 2]>>) -> BuildingFeature {
        BuildingFeature {
            id: Some(id),
            rings,
            ..Default::default()
        }
    }

    fn build_with_cfg(buildings: &[BuildingFeature], cfg: &HeightConfig) -> Mesh {
        let tile = DecodedTile {
            extent: 4096,
            buildings: buildings.to_vec(),
        };
        let sources = [Source {
            z: 14,
            x: 0,
            y: 0,
            tile: &tile,
        }];
        build_mesh(14, 0, 0, &sources, AreaFilter::default(), false, None, cfg)
    }

    fn extract_with_cfg(
        buildings: &[BuildingFeature],
        cfg: &HeightConfig,
    ) -> Vec<ExtractedBuilding> {
        let tile = DecodedTile {
            extent: 4096,
            buildings: buildings.to_vec(),
        };
        let sources = [Source {
            z: 14,
            x: 0,
            y: 0,
            tile: &tile,
        }];
        extract_buildings(&sources, cfg)
    }

    /// Regression pin for the first-fragment-area bug: a two-fragment
    /// building whose merged area lands in a different footprint bucket
    /// than a single fragment's area. The old build_mesh resolved from
    /// the first fragment only and would emit the smaller bucket's
    /// height.
    #[test]
    fn multi_fragment_height_resolves_from_merged_area() {
        let cfg = HeightConfig::default();
        // Two disjoint ~150 m² squares (~12 m side at this tile's
        // latitude): each alone sits in the 60-200 m² bucket, merged
        // ~300 m² crosses into the 200-800 m² bucket.
        let b = bare(7, vec![ring(0, 0, 235), ring(1000, 0, 235)]);
        let mesh = build_with_cfg(&[b], &cfg);

        assert_eq!(mesh.features.len(), 1);
        let f = &mesh.features[0];
        assert_eq!(f.height_method, "footprint");

        let merged = f.footprint_m2;
        let h_merged = cfg.footprint.rural.lookup(merged);
        let h_first = cfg.footprint.rural.lookup(merged / 2.0);
        assert_ne!(
            h_merged, h_first,
            "test geometry must straddle a bucket boundary \
             (merged {merged} m²); adjust ring sizes"
        );
        assert!(
            (f.height_m - h_merged as f32).abs() < 1e-4,
            "height {} must come from the merged area, not a fragment",
            f.height_m
        );
    }

    #[test]
    fn model_on_tags_and_clamps_metadata_less_buildings() {
        let cfg = HeightConfig {
            model: crate::height_config::ModelMode::On,
            ..Default::default()
        };

        let mesh = build_with_cfg(&[bare(8, vec![ring(0, 0, 235)]), square(9)], &cfg);

        let by_id: std::collections::HashMap<_, _> =
            mesh.features.iter().map(|f| (f.feature_id, f)).collect();

        let modeled = by_id[&Some(8)];
        assert_eq!(modeled.height_method, "model");
        let m = crate::height_model::builtin();
        assert!(
            modeled.height_m >= m.model.clamp_min_m && modeled.height_m <= m.model.clamp_max_m,
            "model height {} outside clamp range",
            modeled.height_m
        );

        // Steps 1-2 still bypass the model entirely.
        let explicit = by_id[&Some(9)];
        assert_eq!(explicit.height_method, "explicit");
        assert!((explicit.height_m - 10.0).abs() < 1e-4);
    }

    /// The parity firewall: the renderer (build_mesh) and the evaluator
    /// (extract_buildings) must resolve identical heights for the same
    /// input, in both model modes. Covers every cascade path plus a
    /// multi-fragment building.
    #[test]
    fn build_mesh_and_extract_buildings_resolve_identical_heights() {
        let mut with_floors = bare(11, vec![ring(0, 300, 100)]);
        with_floors.num_floors = Some(3);
        let mut with_class = bare(12, vec![ring(300, 300, 100)]);
        with_class.class = Some("office".to_string());
        let mut with_subtype = bare(13, vec![ring(600, 300, 100)]);
        with_subtype.subtype = Some("residential".to_string());
        let buildings = vec![
            square(10),
            with_floors,
            with_class,
            with_subtype,
            bare(14, vec![ring(900, 300, 100)]),
            bare(15, vec![ring(0, 700, 235), ring(1000, 700, 235)]),
        ];

        for mode in [
            crate::height_config::ModelMode::Off,
            crate::height_config::ModelMode::On,
        ] {
            let cfg = HeightConfig {
                model: mode,
                ..Default::default()
            };

            let mesh = build_with_cfg(&buildings, &cfg);
            let extracted = extract_with_cfg(&buildings, &cfg);
            assert_eq!(mesh.features.len(), buildings.len());
            assert_eq!(extracted.len(), buildings.len());

            let rendered: std::collections::HashMap<_, _> = mesh
                .features
                .iter()
                .map(|f| (f.feature_id, (f.height_m, f.height_method)))
                .collect();
            for e in &extracted {
                let (h, method) = rendered[&e.feature_id];
                assert_eq!(
                    method, e.height_method,
                    "method skew for {:?} in {mode:?}",
                    e.feature_id
                );
                assert!(
                    (h - e.height_m).abs() < 1e-4,
                    "height skew for {:?} in {mode:?}: render {h} vs extract {}",
                    e.feature_id,
                    e.height_m
                );
            }
        }
    }
}
