//! Geographic coordinate utilities.
//!
//! Three frames are in play:
//! * WGS84 geographic: (lon_deg, lat_deg, height_m)
//! * ECEF (EPSG:4978): (X, Y, Z) Cartesian meters from Earth centre
//! * ENU local: east/north/up tangent frame anchored at some lat/lon
//!
//! All buildings within a tile are expressed in ENU metres relative to the
//! tile's centre. A 4x4 affine matrix in the glb root node converts that
//! frame into ECEF for renderers like Cesium.

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
///
/// Returns `(NW, SE)` so x grows east, y grows south.
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

/// WGS84 → ECEF (metres). `height` is ellipsoidal height; for buildings we
/// pass 0 (the ground; buildings extrude upward in the ENU frame).
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

/// Column-major 4x4 affine matrix that maps an ENU point at `origin` into
/// ECEF. glTF's `node.matrix` is column-major.
pub fn enu_to_ecef_matrix(origin: LonLat) -> [f64; 16] {
    let lon = origin.lon_deg.to_radians();
    let lat = origin.lat_deg.to_radians();
    let sin_lon = lon.sin();
    let cos_lon = lon.cos();
    let sin_lat = lat.sin();
    let cos_lat = lat.cos();
    // ENU basis in ECEF coordinates (each row is one ENU axis expressed in ECEF):
    //   east  = (-sinλ,  cosλ, 0)
    //   north = (-sinφ cosλ, -sinφ sinλ, cosφ)
    //   up    = ( cosφ cosλ,  cosφ sinλ, sinφ)
    // We need columns of the transform mapping ENU→ECEF, so columns are
    // east, north, up as ECEF vectors.
    let ex = -sin_lon;
    let ey = cos_lon;
    let ez = 0.0;
    let nx = -sin_lat * cos_lon;
    let ny = -sin_lat * sin_lon;
    let nz = cos_lat;
    let ux = cos_lat * cos_lon;
    let uy = cos_lat * sin_lon;
    let uz = sin_lat;
    let t = wgs84_to_ecef(origin, 0.0);
    // Column-major: col0 = east, col1 = north, col2 = up, col3 = translation
    [
        ex, ey, ez, 0.0, nx, ny, nz, 0.0, ux, uy, uz, 0.0, t[0], t[1], t[2], 1.0,
    ]
}

/// Convert a tile-pixel coordinate to ENU metres relative to the tile centre.
///
/// At z=14 a tile spans ~2.4 km; using a local plane-tangent linearisation
/// keeps sub-decimetre accuracy across the tile, which is plenty for
/// building geometry.
pub fn tile_xy_to_enu(z: u8, x: u32, y: u32, extent: u32, tx: i32, ty: i32) -> [f64; 2] {
    let (nw, se) = tile_bounds(z, x, y);
    let center = LonLat {
        lon_deg: 0.5 * (nw.lon_deg + se.lon_deg),
        lat_deg: 0.5 * (nw.lat_deg + se.lat_deg),
    };
    let u = tx as f64 / extent as f64; // 0..1 west→east
    let v = ty as f64 / extent as f64; // 0..1 north→south
    let lon = nw.lon_deg + (se.lon_deg - nw.lon_deg) * u;
    let lat = nw.lat_deg + (se.lat_deg - nw.lat_deg) * v;
    let cos_lat = center.lat_deg.to_radians().cos();
    let dlon = (lon - center.lon_deg).to_radians();
    let dlat = (lat - center.lat_deg).to_radians();
    let east = dlon * A * cos_lat;
    let north = dlat * A;
    [east, north]
}
