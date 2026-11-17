import ThreeView, { Color, vector3ToGeodetic } from "@navaramap/three";
import { type DefaultDescriptions, DefaultPlugin } from "@navaramap/three-default-plugin";
import { TileJsonPlugin } from "@navaramap/three-plugins";
import type { Map as MaplibreMap } from "maplibre-gl";

const params = new URLSearchParams(location.search);
const lon = Number(params.get("lon") ?? 139.7649);
const lat = Number(params.get("lat") ?? 35.6785);
const height = Number(params.get("height") ?? 1500);
const heading = Number(params.get("heading") ?? 0);
const pitch = Number(params.get("pitch") ?? -30);
const debug = params.get("debug") === "1";
const sse = Number(params.get("sse") ?? 16);

const BUILDING_COLOR = new Color().setHex(0xffffff);
const SELECTED_COLOR = new Color().setHex(0xc95f3d);

const appEl = document.getElementById("app")!;

const view = new ThreeView<DefaultDescriptions>({
  container: appEl,
  // Shadows are init-only, so they have to be asked for here.
  shadow: true,
  // 4× MSAA on the main scene buffer — kills the jaggies along roof edges
  // and tile seams without the blur FXAA introduces.
  multisampling: 4,
  // Render only when something changes (camera, tiles, …). With a pinned
  // sun there's no need for a 60 fps draw loop; idle GPU use drops to ~0.
  animation: false,
  debug,
});
const defaultPlugin = new DefaultPlugin();
view.addPlugin(defaultPlugin);
const tilejson = new TileJsonPlugin();
view.addPlugin(tilejson);
await view.init();

// Pin the sun at 10:00 JST on an autumn morning — low enough that buildings
// cast long, directional shadows for OG screenshots, and independent of when
// the page is actually loaded. The explicit `Z` keeps it off the device's
// timezone.
view.atmosphere.date = new Date("2024-11-01T01:00:00Z");

// Sky and stars, without the photoreal aerial perspective / tone mapping:
// the viewer keeps the flat, paper-map look of the basemap.
view.addMesh({ sky: {} });
view.addMesh({ stars: {} });

// Sun with cascaded shadows on both the terrain and the buildings. The
// 4096² map and normal bias keep shadow acne off tall façades when the sun
// grazes them at the autumn-morning angle.
view.addLight({
  sun: {
    intensity: 2,
    castShadow: true,
    shadowMapSize: 4096,
    shadowFar: 10000,
    shadowNormalBias: 3,
  },
});
// Flat fill, slightly cool like an open sky, so walls facing away from the
// sun don't go black.
view.addLight({ ambient: { color: new Color().setRGB(0.84, 0.88, 0.96), intensity: 0.9 } });
// Contact darkening between adjacent buildings and at the building base,
// which direct lighting alone can't recover.
view.addEffect({ ssao: { intensity: 2 } });

// Re:Earth Terrain quantized-mesh. Render order is add order, so terrain
// goes first and the basemap drapes over it.
const terrain = view.addSource({
  type: "quantized-mesh",
  url: "https://terrain.reearth.land/cesium-mesh/ellipsoid/{z}/{x}/{y}.terrain",
  maxZoom: 18,
  requestVertexNormals: true,
});
view.addLayer({
  type: "terrain",
  source: terrain,
  terrain: { castShadow: true, receiveShadow: true },
});

// Re:Earth Papers basemap. The plugin reads tile URL, zoom range and
// attribution from the TileJSON and puts the credit in view.attribution.
try {
  const basemap = await tilejson.addSource({
    type: "raster-tile",
    url: "https://papers.reearth.land/styles/papers-light/tilejson.json",
  });
  view.addLayer({ type: "raster", source: basemap });
} catch (err) {
  console.warn("basemap load failed", err);
}

view.setCamera({ lng: lon, lat, height, heading, pitch, roll: 0 });

// Absolute URL: Navara resolves the tileset's content URIs inside a worker,
// where a root-relative URL has no page to resolve against.
const TILESET_URL = new URL("/tileset.json", location.href).href;
const buildingsSource = view.addSource({ type: "3d-tiles", url: TILESET_URL });
const buildings = view.addLayer({
  type: "3d-tiles",
  source: buildingsSource,
  model: {
    color: BUILDING_COLOR,
    metalness: 0,
    roughness: 1,
    castShadow: true,
    receiveShadow: true,
    maxSse: sse,
    showBoundingBox: debug,
  },
});

view.attribution?.add([
  { attribution: "© Re:Earth Terrain", attributionUrl: "https://terrain.reearth.land/" },
]);
// tileset.json's asset.copyright is the single source of truth for the
// building credit. Navara doesn't read it from the tileset, so add it here.
fetch(TILESET_URL)
  .then((r) => r.json())
  .then((t: { asset?: { copyright?: string } }) => {
    const cp = t?.asset?.copyright;
    if (cp) view.attribution?.add([{ attributionHtml: cp }]);
  })
  .catch((err) => console.warn("attribution fetch failed", err));

// Expose for debugging via DevTools console.
Object.assign(window, { view, buildings });

// ----- Building info panel -----
// Clicking a building lists its EXT_structural_metadata properties and
// tints it; clicking empty space (or ×) clears both.
const infoEl = document.getElementById("info")!;
const infoBody = document.getElementById("info-body")!;
let selectedId: string | undefined;

buildings.on("featureUpdated", ({ evaluator }) => {
  evaluator.evaluate(
    ({ properties }) => ({
      // Return the base color for everything else too: an evaluated value
      // sticks until the evaluator overrides it.
      color:
        selectedId !== undefined && properties?.gers_id === selectedId
          ? SELECTED_COLOR
          : BUILDING_COLOR,
    }),
    { filters: ["gers_id"] },
  );
});

function select(properties: Record<string, unknown> | undefined) {
  const id = properties?.gers_id;
  selectedId = typeof id === "string" ? id : undefined;
  buildings.forceUpdate();
  if (!properties) {
    infoEl.classList.remove("open");
    return;
  }
  infoBody.replaceChildren(
    ...Object.entries(properties).map(([key, value]) => {
      const tr = document.createElement("tr");
      const th = document.createElement("th");
      const td = document.createElement("td");
      th.textContent = key;
      td.textContent = String(value);
      tr.append(th, td);
      return tr;
    }),
  );
  infoEl.classList.add("open");
}

view.on("featureClick", (info) => {
  select(info?.layerId === buildings.id ? info.properties : undefined);
});
document.getElementById("info-close")!.addEventListener("click", () => select(undefined));

// ----- Get-started modal -----
const modal = document.getElementById("modal-backdrop")!;
const openModal = () => modal.classList.add("open");
const closeModal = () => modal.classList.remove("open");
document.getElementById("btn-get-started")!.addEventListener("click", openModal);
document.getElementById("modal-close")!.addEventListener("click", closeModal);
modal.addEventListener("click", (e) => {
  if (e.target === modal) closeModal();
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && modal.classList.contains("open")) closeModal();
});

for (const tab of document.querySelectorAll<HTMLButtonElement>(".tabs [role=tab]")) {
  tab.addEventListener("click", () => {
    for (const other of document.querySelectorAll<HTMLButtonElement>(".tabs [role=tab]")) {
      const selected = other === tab;
      other.setAttribute("aria-selected", String(selected));
      const panel = document.getElementById(other.getAttribute("aria-controls") ?? "");
      if (panel) panel.hidden = !selected;
    }
  });
}

for (const btn of document.querySelectorAll<HTMLButtonElement>(".codeblock .copy")) {
  btn.addEventListener("click", async () => {
    const target = document.getElementById(btn.dataset.target ?? "");
    const text = target?.dataset.copyText ?? target?.textContent ?? "";
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Clipboard API can fail on insecure origins; fall back to a hidden
      // textarea selection so the button still works.
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      try {
        document.execCommand("copy");
      } finally {
        ta.remove();
      }
    }
    const original = btn.textContent;
    btn.textContent = "Copied";
    btn.classList.add("copied");
    setTimeout(() => {
      btn.textContent = original;
      btn.classList.remove("copied");
    }, 1500);
  });
}

// ----- Compare split view (debug) -----
// Clicking "Compare" splits the screen: the 3D view (left) drops to a
// straight-down nadir view with tilt locked, and a MapLibre map (right)
// renders the raw Overture `building` footprints via the pmtiles://
// protocol (proxied through /debug/overture.pmtiles). The two cameras are
// kept in sync both ways so you can pan/zoom either panel and watch the
// 3D Tiles output track the source footprints.
const btnCompare = document.getElementById("btn-compare")!;
const compareStatus = document.getElementById("compare-status")!;

const MPP0 = 40075016.686 / 512; // metres/pixel at zoom 0 on the equator
const D2R = Math.PI / 180;
const zoomForMpp = (mpp: number, lat: number) => Math.log2((MPP0 * Math.cos(lat * D2R)) / mpp);
const mppForZoom = (zoom: number, lat: number) => (MPP0 * Math.cos(lat * D2R)) / 2 ** zoom;
// Vertical field of view in radians (Navara reports degrees).
const fovy = () => {
  const f = view.camera.fov;
  return f !== undefined && Number.isFinite(f) ? f * D2R : Math.PI / 3;
};
// Normalise a bearing difference into (-180, 180].
const bearingDelta = (a: number, b: number) => ((((a - b) % 360) + 540) % 360) - 180;

let map: MaplibreMap | null = null;
let splitActive = false;
let savedView: ReturnType<typeof currentView> | null = null; // restored when leaving split

function currentView() {
  const { lng, lat, height } = view.camera.positionGeographic;
  const { heading = 0, pitch = 0, roll = 0 } = view.camera.orientation;
  return { lng, lat, height, heading, pitch, roll };
}

// The two sides are kept in sync without re-entrancy flags: each direction
// is idempotent and only writes when the target would actually move by more
// than a sub-pixel epsilon. A propagated change lands within epsilon on the
// return trip, so it doesn't bounce back.

// 3D → MapLibre: match the map's vertical ground extent (metres) to what
// the nadir camera sees, so both panels frame the same area.
function syncMapFromView() {
  if (!map) return;
  const cam = currentView();
  const groundH = 2 * cam.height * Math.tan(fovy() / 2);
  const mapH = map.getCanvas().clientHeight || 1;
  const zoom = zoomForMpp(groundH / mapH, cam.lat);
  const cur = map.getCenter();
  if (
    Math.abs(cam.lng - cur.lng) < 1e-6 &&
    Math.abs(cam.lat - cur.lat) < 1e-6 &&
    Math.abs(zoom - map.getZoom()) < 2e-3 &&
    Math.abs(bearingDelta(cam.heading, map.getBearing())) < 2e-2
  ) {
    return; // already there — don't echo
  }
  map.jumpTo({ center: [cam.lng, cam.lat], zoom, bearing: cam.heading });
}

// MapLibre → 3D: invert the same relation, forcing the camera nadir.
function syncViewFromMap() {
  if (!map) return;
  const c = map.getCenter();
  const mapH = map.getCanvas().clientHeight || 1;
  const groundH = mppForZoom(map.getZoom(), c.lat) * mapH;
  const h = groundH / (2 * Math.tan(fovy() / 2));
  const bearing = map.getBearing();
  const cam = currentView();
  if (
    Math.abs(c.lng - cam.lng) < 1e-6 &&
    Math.abs(c.lat - cam.lat) < 1e-6 &&
    Math.abs(h - cam.height) < Math.max(0.5, h * 1e-3) &&
    Math.abs(bearingDelta(bearing, cam.heading)) < 3e-2
  ) {
    return; // already there — don't echo
  }
  view.setCamera({ lng: c.lng, lat: c.lat, height: h, heading: bearing, pitch: -90, roll: 0 });
}

// `move` covers both gestures and programmatic jumps (setCamera emits
// move + moveend in the same frame, which the epsilon check absorbs).
view.camera.on("move", () => {
  if (splitActive) syncMapFromView();
});

async function ensureMap(): Promise<MaplibreMap> {
  if (map) return map;
  compareStatus.textContent = " — loading…";
  // Lazy-load MapLibre + pmtiles only when compare is first opened.
  const [{ default: maplibregl }, { Protocol }] = await Promise.all([
    import("maplibre-gl"),
    import("pmtiles"),
    import("maplibre-gl/dist/maplibre-gl.css"),
  ]);
  const protocol = new Protocol();
  maplibregl.addProtocol("pmtiles", protocol.tile);

  const m = new maplibregl.Map({
    container: "compare-map",
    style: {
      version: 8,
      sources: {
        // No basemap — the 3D view gives the spatial context; the dark
        // background keeps the raw footprints legible. ODbL data is
        // OSM-derived, so attribute both.
        overture: {
          type: "vector",
          url: `pmtiles://${location.origin}/debug/overture.pmtiles`,
          attribution: "© OpenStreetMap contributors, Overture Maps Foundation (ODbL)",
        },
      },
      layers: [
        { id: "bg", type: "background", paint: { "background-color": "#161e29" } },
        {
          id: "ov-fill",
          type: "fill",
          source: "overture",
          "source-layer": "building",
          paint: { "fill-color": "#c95f3d", "fill-opacity": 0.9 },
        },
        {
          id: "ov-line",
          type: "line",
          source: "overture",
          "source-layer": "building",
          paint: { "line-color": "#f2efe8", "line-width": 0.4 },
        },
      ],
    },
    // Stay 2D/top-down to mirror the 3D view's nadir camera.
    pitch: 0,
    maxPitch: 0,
    pitchWithRotate: false,
    attributionControl: { compact: true },
  });
  m.on("move", () => {
    if (splitActive) syncViewFromMap();
  });
  m.on("error", (e) => console.warn("compare map error", e?.error ?? e));
  await new Promise((res) => m.on("load", res));
  compareStatus.textContent = "";
  map = m;
  Object.assign(window, { compareMap: m }); // expose for debugging via DevTools console
  return m;
}

async function enterSplit() {
  // Drop to a straight-down, north-up view over whatever the camera is
  // currently looking at, and lock tilt so it stays top-down while you
  // drag. Forcing heading 0 keeps the comparison north-up: otherwise
  // entering compare while the 3D view was rotated left both panels
  // showing south-up. Pick before resizing: right after a resize the pick
  // finds no terrain until the next frame has rendered.
  savedView = currentView();
  const rect = view.canvas.getBoundingClientRect();
  const ground = view.pickTerrainPosition(rect.left + rect.width / 2, rect.top + rect.height / 2);
  const target = ground ? vector3ToGeodetic(ground) : savedView;

  document.body.classList.add("split");
  btnCompare.classList.add("active");
  view.resize();
  view.setCamera({
    lng: target.lng,
    lat: target.lat,
    height: savedView.height,
    heading: 0,
    pitch: -90,
    roll: 0,
  });
  view.camera.options = { enableTilt: false };

  let m: MaplibreMap;
  try {
    m = await ensureMap();
  } catch (err) {
    console.error("compare init failed", err);
    compareStatus.textContent = " — failed to load";
    return;
  }
  m.resize();
  splitActive = true;
  // Seed the map from the (now nadir) camera.
  syncMapFromView();
}

function exitSplit() {
  splitActive = false;
  document.body.classList.remove("split");
  btnCompare.classList.remove("active");
  view.camera.options = { enableTilt: true };
  if (savedView) {
    view.setCamera(savedView);
    savedView = null;
  }
  view.resize();
}

btnCompare.addEventListener("click", () => {
  if (splitActive) exitSplit();
  else enterSplit();
});
