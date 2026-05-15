//! Geographic coordinate utilities.
//!
//! Three frames are in play:
//! * WGS84 geographic: (lon_deg, lat_deg, height_m)
//! * ECEF (EPSG:4978): (X, Y, Z) Cartesian meters from Earth centre
//! * ENU local: east/north/up tangent frame anchored at some lat/lon
//!
//! For LOD we let one tile's geometry live in a different tile's ENU
//! frame: convert tile pixel → lon/lat (source tile's frame), then
//! lon/lat → ENU offset against an arbitrary anchor (output tile's
//! centre). All buildings in a single glb share that anchor; a 4x4
//! affine in the glb's root node maps ENU→ECEF for renderers.

// WGS84 ellipsoid
const A: f64 = 6_378_137.0;
const F: f64 = 1.0 / 298.257_223_563;
const E2: f64 = F * (2.0 - F);

#[derive(Debug, Clone, Copy)]
pub struct LonLat {
    pub lon_deg: f64,
    pub lat_deg: f64,
}

/// Web-Mercator slippy-tile bounds in lon/lat degrees.
pub fn tile_bounds(z: u8, x: u32, y: u32) -> (LonLat, LonLat) {
    let n = (1u64 << z) as f64;
    let lon_w = x as f64 / n * 360.0 - 180.0;
    let lon_e = (x as f64 + 1.0) / n * 360.0 - 180.0;
    let lat_n_rad = ((std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n)).sinh()).atan();
    let lat_s_rad = ((std::f64::consts::PI * (1.0 - 2.0 * (y as f64 + 1.0) / n)).sinh()).atan();
    (
        LonLat {
            lon_deg: lon_w,
            lat_deg: lat_n_rad.to_degrees(),
        },
        LonLat {
            lon_deg: lon_e,
            lat_deg: lat_s_rad.to_degrees(),
        },
    )
}

pub fn tile_center(z: u8, x: u32, y: u32) -> LonLat {
    let (nw, se) = tile_bounds(z, x, y);
    LonLat {
        lon_deg: 0.5 * (nw.lon_deg + se.lon_deg),
        lat_deg: 0.5 * (nw.lat_deg + se.lat_deg),
    }
}

/// WGS84 → ECEF metres.
pub fn wgs84_to_ecef(p: LonLat, height: f64) -> [f64; 3] {
    let lon = p.lon_deg.to_radians();
    let lat = p.lat_deg.to_radians();
    let sin_lat = lat.sin();
    let cos_lat = lat.cos();
    let n = A / (1.0 - E2 * sin_lat * sin_lat).sqrt();
    let x = (n + height) * cos_lat * lon.cos();
    let y = (n + height) * cos_lat * lon.sin();
    let z = (n * (1.0 - E2) + height) * sin_lat;
    [x, y, z]
}

/// ECEF metres → WGS84 (lon/lat in degrees, height in metres). Closed-form
/// Bowring solution; sub-millimetre accuracy at terrestrial radii.
pub fn ecef_to_wgs84(p: [f64; 3]) -> (LonLat, f64) {
    let (x, y, z) = (p[0], p[1], p[2]);
    let b = A * (1.0 - F);
    let ep2 = (A * A - b * b) / (b * b);
    let r = (x * x + y * y).sqrt();
    let theta = (z * A).atan2(r * b);
    let sin_t = theta.sin();
    let cos_t = theta.cos();
    let lat = (z + ep2 * b * sin_t * sin_t * sin_t).atan2(r - E2 * A * cos_t * cos_t * cos_t);
    let lon = y.atan2(x);
    let n = A / (1.0 - E2 * lat.sin() * lat.sin()).sqrt();
    let h = r / lat.cos() - n;
    (
        LonLat {
            lon_deg: lon.to_degrees(),
            lat_deg: lat.to_degrees(),
        },
        h,
    )
}

/// Column-major 4x4 ENU→ECEF affine anchored at `origin`. glTF's
/// `node.matrix` is column-major. Origin sits on the WGS84 ellipsoid
/// (h = 0); use [`enu_to_ecef_matrix_at_height`] to lift it to the
/// geoid (sea level) or another reference surface.
pub fn enu_to_ecef_matrix(origin: LonLat) -> [f64; 16] {
    enu_to_ecef_matrix_at_height(origin, 0.0)
}

/// Same as [`enu_to_ecef_matrix`] but anchors the origin at ellipsoidal
/// height `height_m`. Pass `N` (geoid undulation, EGM2008) to make the
/// ENU frame sit on mean sea level rather than the bare ellipsoid.
pub fn enu_to_ecef_matrix_at_height(origin: LonLat, height_m: f64) -> [f64; 16] {
    let lon = origin.lon_deg.to_radians();
    let lat = origin.lat_deg.to_radians();
    let sin_lon = lon.sin();
    let cos_lon = lon.cos();
    let sin_lat = lat.sin();
    let cos_lat = lat.cos();
    let ex = -sin_lon;
    let ey = cos_lon;
    let ez = 0.0;
    let nx = -sin_lat * cos_lon;
    let ny = -sin_lat * sin_lon;
    let nz = cos_lat;
    let ux = cos_lat * cos_lon;
    let uy = cos_lat * sin_lon;
    let uz = sin_lat;
    let t = wgs84_to_ecef(origin, height_m);
    [
        ex, ey, ez, 0.0, nx, ny, nz, 0.0, ux, uy, uz, 0.0, t[0], t[1], t[2], 1.0,
    ]
}

/// Geographic position of (`tx`, `ty`) within tile (`z`, `x`, `y`). The
/// integer pixel coords are in [0, extent].
pub fn tile_xy_to_lonlat(z: u8, x: u32, y: u32, extent: u32, tx: i32, ty: i32) -> LonLat {
    let (nw, se) = tile_bounds(z, x, y);
    let u = tx as f64 / extent as f64; // 0..1 west→east
    let v = ty as f64 / extent as f64; // 0..1 north→south
    LonLat {
        lon_deg: nw.lon_deg + (se.lon_deg - nw.lon_deg) * u,
        lat_deg: nw.lat_deg + (se.lat_deg - nw.lat_deg) * v,
    }
}

/// ENU offset (east, north) in metres from `anchor` to `p`. At z=14
/// (~2.4 km tile) the plane-tangent linearisation keeps sub-decimetre
/// accuracy, more than enough for building geometry.
pub fn lonlat_to_enu_at(p: LonLat, anchor: LonLat) -> [f64; 2] {
    let cos_lat = anchor.lat_deg.to_radians().cos();
    let dlon = (p.lon_deg - anchor.lon_deg).to_radians();
    let dlat = (p.lat_deg - anchor.lat_deg).to_radians();
    [dlon * A * cos_lat, dlat * A]
}

/// Convenience: tile pixel → ENU offset against an arbitrary anchor.
pub fn tile_xy_to_enu_at(
    src_z: u8,
    src_x: u32,
    src_y: u32,
    extent: u32,
    tx: i32,
    ty: i32,
    anchor: LonLat,
) -> [f64; 2] {
    lonlat_to_enu_at(
        tile_xy_to_lonlat(src_z, src_x, src_y, extent, tx, ty),
        anchor,
    )
}
