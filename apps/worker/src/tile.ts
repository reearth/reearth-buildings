// Slippy-tile math shared by tileset.ts and other helpers.

export const toRad = (d: number) => (d * Math.PI) / 180;

export const lonToX = (lon: number, z: number) => Math.floor(((lon + 180) / 360) * 2 ** z);

export const latToY = (lat: number, z: number) => {
  const r = (lat * Math.PI) / 180;
  return Math.floor(((1 - Math.log(Math.tan(r) + 1 / Math.cos(r)) / Math.PI) / 2) * 2 ** z);
};

export const xToLon = (x: number, z: number) => (x / 2 ** z) * 360 - 180;

export const yToLat = (y: number, z: number) => {
  const n = Math.PI - (2 * Math.PI * y) / 2 ** z;
  return (180 / Math.PI) * Math.atan((Math.exp(n) - Math.exp(-n)) / 2);
};

/** Bounding region for tile (z, x, y) as [west, south, east, north, min_h, max_h] radians. */
export function tileRegion(z: number, x: number, y: number, minH: number, maxH: number): number[] {
  return [
    toRad(xToLon(x, z)),
    toRad(yToLat(y + 1, z)),
    toRad(xToLon(x + 1, z)),
    toRad(yToLat(y, z)),
    minH,
    maxH,
  ];
}
