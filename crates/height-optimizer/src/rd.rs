//! Rijksdriehoek (EPSG:28992, "RD New") ↔ WGS84 conversion.
//!
//! 3D BAG data and its spatial index live in RD New metres, so bbox
//! queries must be issued in RD and results converted back to lon/lat.
//! No projection library is vendored in the workspace, so we transcribe
//! the Schreutelkamp / Strang van Hees approximating polynomials (accurate
//! to ~decimetres across the Netherlands — far tighter than the ~metre
//! matching tolerance we need for truth centroids).
//!
//! Coefficients transcribed verbatim from the `rijksdriehoek` crate
//! (SahiBkom/rijksdriehoek, `src/lib.rs`, MIT/Apache-2.0), which in turn
//! sources them from F. H. Schreutelkamp & G. L. Strang van Hees,
//! "Benaderingsformules voor de transformatie tussen RD- en
//! WGS84-kaartcoördinaten". The Amersfoort datum point anchors both
//! directions.

/// Amersfoort datum point — RD false origin.
const REF_RD_X: f64 = 155000.0;
const REF_RD_Y: f64 = 463000.0;
/// Amersfoort datum point — WGS84 (latitude, longitude in degrees).
const REF_LAT: f64 = 52.15517;
const REF_LON: f64 = 5.387206;

/// RD → WGS84 latitude coefficients as `(p, q, k)`: contribute
/// `k · dx^p · dy^q` arc-seconds of latitude, where `dx`/`dy` are the RD
/// offset from Amersfoort scaled by 1e-5.
const K_PQ: [(i32, i32, f64); 11] = [
    (0, 1, 3235.65389),
    (2, 0, -32.58297),
    (0, 2, -0.24750),
    (2, 1, -0.84978),
    (0, 3, -0.06550),
    (2, 2, -0.01709),
    (1, 0, -0.00738),
    (4, 0, 0.00530),
    (2, 3, -0.00039),
    (4, 1, 0.00033),
    (1, 1, -0.00012),
];

/// RD → WGS84 longitude coefficients as `(p, q, l)`: contribute
/// `l · dx^p · dy^q` arc-seconds of longitude.
const L_PQ: [(i32, i32, f64); 12] = [
    (1, 0, 5260.52916),
    (1, 1, 105.94684),
    (1, 2, 2.45656),
    (3, 0, -0.81885),
    (1, 3, 0.05594),
    (3, 1, -0.05607),
    (0, 1, 0.01199),
    (3, 2, -0.00256),
    (1, 4, 0.00128),
    (0, 2, 0.00022),
    (2, 0, -0.00022),
    (5, 0, 0.00026),
];

/// WGS84 → RD easting coefficients `R[p][q]`: contribute
/// `R · dphi^p · dlam^q` metres of easting, where `dphi`/`dlam` are the
/// degree offset from Amersfoort scaled by 0.36.
const R_PQ: [[f64; 5]; 4] = [
    [0.0, 190094.945, -0.008, -32.391, 0.0],
    [-0.705, -11832.228, 0.0, -0.608, 0.0],
    [0.0, -114.221, 0.0, 0.148, 0.0],
    [0.0, -2.340, 0.0, 0.0, 0.0],
];

/// WGS84 → RD northing coefficients `S[p][q]`: contribute
/// `S · dphi^p · dlam^q` metres of northing.
const S_PQ: [[f64; 5]; 4] = [
    [0.0, 0.433, 3638.893, 0.0, 0.092],
    [309056.544, -0.032, -157.984, 0.0, -0.054],
    [73.077, 0.0, -6.439, 0.0, 0.0],
    [59.788, 0.0, 0.0, 0.0, 0.0],
];

/// Convert RD New metres to WGS84 `(lon_deg, lat_deg)`.
pub fn rd_to_wgs84(x: f64, y: f64) -> (f64, f64) {
    let dx = (x - REF_RD_X) * 1e-5;
    let dy = (y - REF_RD_Y) * 1e-5;

    let mut lat_sec = 0.0;
    for (p, q, k) in K_PQ {
        lat_sec += k * dx.powi(p) * dy.powi(q);
    }
    let mut lon_sec = 0.0;
    for (p, q, l) in L_PQ {
        lon_sec += l * dx.powi(p) * dy.powi(q);
    }

    let lat = REF_LAT + lat_sec / 3600.0;
    let lon = REF_LON + lon_sec / 3600.0;
    (lon, lat)
}

/// Convert WGS84 `(lon_deg, lat_deg)` to RD New metres `(x, y)`.
pub fn wgs84_to_rd(lon: f64, lat: f64) -> (f64, f64) {
    let dphi = 0.36 * (lat - REF_LAT);
    let dlam = 0.36 * (lon - REF_LON);

    let mut x = REF_RD_X;
    let mut y = REF_RD_Y;
    for p in 0..R_PQ.len() {
        for q in 0..R_PQ[p].len() {
            let t = dphi.powi(p as i32) * dlam.powi(q as i32);
            x += R_PQ[p][q] * t;
            y += S_PQ[p][q] * t;
        }
    }
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One arc-second of latitude is ~30.9 m; convert a degree error to an
    /// approximate ground distance in metres at Dutch latitudes.
    fn deg_err_m(dlon: f64, dlat: f64, lat: f64) -> f64 {
        let m_per_deg_lat = 111_132.0;
        let m_per_deg_lon = 111_320.0 * lat.to_radians().cos();
        ((dlon * m_per_deg_lon).powi(2) + (dlat * m_per_deg_lat).powi(2)).sqrt()
    }

    #[test]
    fn westertoren_rd_to_wgs84() {
        let (lon, lat) = rd_to_wgs84(120700.723, 487525.501);
        assert!(
            deg_err_m(lon - 4.88352559, lat - 52.37453253, 52.37) < 1.0,
            "lon={lon} lat={lat}"
        );
    }

    #[test]
    fn westertoren_wgs84_to_rd() {
        let (x, y) = wgs84_to_rd(4.88352559, 52.37453253);
        assert!(
            ((x - 120700.723).powi(2) + (y - 487525.501).powi(2)).sqrt() < 1.0,
            "x={x} y={y}"
        );
    }

    #[test]
    fn martinitoren_rd_to_wgs84() {
        let (lon, lat) = rd_to_wgs84(233883.131, 582065.167);
        assert!(
            deg_err_m(lon - 6.56820053, lat - 53.21938317, 53.22) < 1.0,
            "lon={lon} lat={lat}"
        );
    }

    #[test]
    fn martinitoren_wgs84_to_rd() {
        let (x, y) = wgs84_to_rd(6.56820053, 53.21938317);
        assert!(
            ((x - 233883.131).powi(2) + (y - 582065.167).powi(2)).sqrt() < 1.0,
            "x={x} y={y}"
        );
    }

    #[test]
    fn round_trip_grid_under_1m() {
        // Sample a grid spanning the Netherlands; every round trip must
        // return within a metre.
        let mut worst = 0.0f64;
        for xi in 0..=10 {
            for yi in 0..=10 {
                let x = 15000.0 + xi as f64 * 27000.0; // ~15k..285k
                let y = 305000.0 + yi as f64 * 30000.0; // ~305k..605k
                let (lon, lat) = rd_to_wgs84(x, y);
                let (x2, y2) = wgs84_to_rd(lon, lat);
                let d = ((x2 - x).powi(2) + (y2 - y).powi(2)).sqrt();
                worst = worst.max(d);
            }
        }
        assert!(worst < 1.0, "worst round-trip drift {worst} m");
    }
}
