#!/usr/bin/env bash
# Download the EGM2008 geoid undulation grid as a Cloud Optimized GeoTIFF.
#
# Source: PROJ data CDN. The file is already a COG (Float32, DEFLATE,
# 256x256 tiled, EPSG:4979), so no GDAL post-processing is required — we
# just save it as-is, then upload it to the SOURCES R2 bucket.
#
# Usage:
#   bash apps/worker/scripts/fetch-egm08.sh           # writes data/egm08_cog.tif
#   FORCE=1 bash apps/worker/scripts/fetch-egm08.sh   # re-download
#
# After fetching, upload to R2 (the worker reads from this exact path):
#   wrangler r2 object put reearth-buildings-sources-dev/geoid/egm08_cog.tif \
#     --file apps/worker/data/egm08_cog.tif
# (and equivalent for the staging / prod buckets).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATA_DIR="$ROOT/data"
SRC_URL="https://cdn.proj.org/us_nga_egm08_25.tif"
OUT_FILE="$DATA_DIR/egm08_cog.tif"

FORCE="${FORCE:-0}"

command -v curl >/dev/null 2>&1 || { echo "error: curl required" >&2; exit 1; }

mkdir -p "$DATA_DIR"

if [[ -s "$OUT_FILE" && "$FORCE" != "1" ]]; then
  echo "[fetch-egm08] already present: $OUT_FILE (set FORCE=1 to re-download)"
  exit 0
fi

echo "[fetch-egm08] downloading $SRC_URL"
curl -fL --retry 3 --retry-delay 2 -o "$OUT_FILE.tmp" "$SRC_URL"
mv "$OUT_FILE.tmp" "$OUT_FILE"

echo
echo "[fetch-egm08] done"
ls -la "$OUT_FILE"
echo
echo "Next: upload to R2"
echo "  wrangler r2 object put reearth-buildings-sources-dev/geoid/egm08_cog.tif \\"
echo "    --file $OUT_FILE"
