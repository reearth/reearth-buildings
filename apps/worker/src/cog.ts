// COG (Cloud Optimized GeoTIFF) reader backed by an R2 bucket.
// Mirrors the pattern in eukarya-inc/reearth-terrain: implement geotiff.js's
// loose Source interface so reads come from R2 byte-range fetches rather
// than streaming the whole file.

import GeoTIFF, { type GeoTIFFImage } from "geotiff";

interface Slice {
  offset: number;
  length: number;
}

class R2CogSource {
  #bucket: R2Bucket;
  #key: string;
  #fileSize: number | null = null;

  constructor(bucket: R2Bucket, key: string) {
    this.#bucket = bucket;
    this.#key = key;
  }

  async fetch(slices: Slice[], _signal?: AbortSignal): Promise<ArrayBufferLike[]> {
    return Promise.all(slices.map((s) => this.#fetchSlice(s)));
  }

  async #fetchSlice(slice: Slice): Promise<ArrayBufferLike> {
    const obj = await this.#bucket.get(this.#key, {
      range: { offset: slice.offset, length: slice.length },
    });
    if (!obj) throw new Error(`R2 object not found: ${this.#key}`);
    if (this.#fileSize === null && obj.size != null) this.#fileSize = obj.size;
    return obj.arrayBuffer();
  }

  get fileSize(): number | null {
    return this.#fileSize;
  }

  async close(): Promise<void> {
    /* nothing to release */
  }
}

export interface OpenedCog {
  tiff: GeoTIFF;
  image: GeoTIFFImage;
}

/** Open a COG stored in R2 and return its first (highest-resolution) image. */
export async function openCog(bucket: R2Bucket, key: string): Promise<OpenedCog> {
  const source = new R2CogSource(bucket, key);
  const tiff = await GeoTIFF.fromSource(
    source as unknown as Parameters<typeof GeoTIFF.fromSource>[0],
  );
  const image = await tiff.getImage();
  return { tiff, image };
}
