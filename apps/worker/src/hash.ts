/**
 * Short hex prefix of a SHA-1 digest. Used as a content fingerprint for
 * ETag and R2 cache keys — 8 bytes / 16 hex chars gives a 2^-64 collision
 * floor, which is comfortable for a deduplication cache (a collision just
 * means an unrelated tile gets reused, and we'd notice in QA).
 */
export async function sha1Hex(data: Uint8Array, byteLen = 8): Promise<string> {
  const buf = await crypto.subtle.digest("SHA-1", data);
  const view = new Uint8Array(buf, 0, byteLen);
  let out = "";
  for (let i = 0; i < view.length; i++) {
    out += view[i]!.toString(16).padStart(2, "0");
  }
  return out;
}
