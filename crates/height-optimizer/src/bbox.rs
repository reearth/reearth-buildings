//! Lon/lat axis-aligned bounding box, plus 3D Tiles `region` helpers.

#[derive(Debug, Clone, Copy)]
pub struct BBox {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl BBox {
    pub fn new(west: f64, south: f64, east: f64, north: f64) -> Self {
        Self { west, south, east, north }
    }

    pub fn contains_lonlat(&self, lon: f64, lat: f64) -> bool {
        lon >= self.west && lon <= self.east && lat >= self.south && lat <= self.north
    }

    pub fn intersects(&self, other: &BBox) -> bool {
        !(other.east < self.west
            || other.west > self.east
            || other.north < self.south
            || other.south > self.north)
    }
}

/// Convert a 3D Tiles `boundingVolume.region` array (radians + metres) to
/// a degree-space [`BBox`]. Returns `None` if the input isn't a 6-element
/// number array.
pub fn region_to_bbox(region: &[f64]) -> Option<BBox> {
    if region.len() < 4 {
        return None;
    }
    Some(BBox {
        west: region[0].to_degrees(),
        south: region[1].to_degrees(),
        east: region[2].to_degrees(),
        north: region[3].to_degrees(),
    })
}
