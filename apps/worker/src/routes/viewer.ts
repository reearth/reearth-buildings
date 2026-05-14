import type { Context } from "hono";

// Minimal Cesium viewer for dogfooding. Loads our /tileset.json on top of
// the OpenStreetMap basemap (no Ion token required). Camera initial state
// is overridable via query params:
//   /?lon=139.7&lat=35.68&height=2000&heading=0&pitch=-30
// Add `?debug=1` to draw tileset bounding volumes and surface tile errors
// in the status panel.
//
// Building click reveals attributes via a tiny side panel.

const HTML = `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>Re:Earth Buildings — viewer</title>
<meta name="viewport" content="width=device-width,initial-scale=1" />
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/cesium@1.141/Build/Cesium/Widgets/widgets.css" />
<style>
  html, body, #app { width: 100%; height: 100%; margin: 0; padding: 0; overflow: hidden; }
  .overlay {
    position: absolute; background: rgba(20, 22, 28, 0.92); color: #fff;
    padding: 10px 12px; border-radius: 8px;
    font: 12px/1.45 -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
    box-shadow: 0 4px 20px rgba(0,0,0,0.3);
  }
  #panel { top: 12px; right: 12px; max-width: 320px; display: none; }
  #panel h2 { margin: 0 0 6px; font-size: 13px; font-weight: 600; color: #9fd1ff; }
  #panel dl { margin: 0; display: grid; grid-template-columns: max-content 1fr; gap: 2px 10px; }
  #panel dt { color: #8b95a4; }
  #panel dd { margin: 0; word-break: break-all; }
  #status { bottom: 12px; left: 12px; max-width: 360px; }
  #status .row { margin-top: 2px; color: #cfd6e0; }
  #status .err { color: #ff8e8e; }
  #status .pill { display: inline-block; min-width: 30px; padding: 1px 6px; margin-right: 6px;
                  background: #2a2f3a; border-radius: 99px; text-align: center; }
  #toggle { top: 12px; left: 12px; display: flex; gap: 6px; align-items: center; }
  #toggle button {
    background: #2a2f3a; color: #fff; border: 1px solid #404652;
    padding: 5px 10px; border-radius: 6px; font: inherit; cursor: pointer;
  }
  #toggle button.active { background: #3a82e0; border-color: #3a82e0; }
  #toggle button[disabled] { opacity: 0.5; cursor: not-allowed; }
  #toggle .note { color: #8b95a4; font-size: 11px; }
</style>
</head>
<body>
<div id="app"></div>
<div id="panel" class="overlay"><h2 id="panel-title">Building</h2><dl id="panel-body"></dl></div>
<div id="status" class="overlay">
  <div><span class="pill" id="loaded">0</span>tiles loaded</div>
  <div><span class="pill" id="failed">0</span>tiles failed</div>
  <div class="row" id="last-error"></div>
</div>
<div id="toggle" class="overlay">
  <button id="btn-ours" class="active" type="button">ours</button>
  <button id="btn-cesium" type="button">Cesium OSM</button>
  <span class="note" id="toggle-note"></span>
</div>
<script type="module">
  import * as Cesium from "https://cdn.jsdelivr.net/npm/cesium@1.141/Build/Cesium/index.js";

  window.CESIUM_BASE_URL = "https://cdn.jsdelivr.net/npm/cesium@1.141/Build/Cesium/";

  const params = new URLSearchParams(location.search);
  const lon = Number(params.get("lon") ?? 139.7649);
  const lat = Number(params.get("lat") ?? 35.6785);
  const height = Number(params.get("height") ?? 1500);
  const heading = Number(params.get("heading") ?? 0);
  const pitch = Number(params.get("pitch") ?? -30);
  const debug = params.get("debug") === "1";
  const sse = Number(params.get("sse") ?? 16);
  // Custom Cesium Ion token (for the OSM Buildings comparison toggle).
  // The SDK ships with a default token suitable for dev evaluation but
  // it has lower rate limits; supply your own via ?ion=... for stable
  // testing.
  const ionToken = params.get("ion");
  if (ionToken) Cesium.Ion.defaultAccessToken = ionToken;

  const viewer = new Cesium.Viewer("app", {
    baseLayer: Cesium.ImageryLayer.fromProviderAsync(
      Promise.resolve(new Cesium.OpenStreetMapImageryProvider({ url: "https://tile.openstreetmap.org/" })),
    ),
    baseLayerPicker: false, geocoder: false, homeButton: false,
    sceneModePicker: false, navigationHelpButton: false,
    timeline: false, animation: false, fullscreenButton: false,
  });
  // Enable Cesium's sun-tracking lighting on the globe; side walls were
  // appearing black because the default ambient term alone left them
  // unlit when the sun's dot product with the normal was ~0.
  viewer.scene.globe.enableLighting = true;
  viewer.camera.setView({
    destination: Cesium.Cartesian3.fromDegrees(lon, lat, height),
    orientation: {
      heading: Cesium.Math.toRadians(heading),
      pitch: Cesium.Math.toRadians(pitch),
      roll: 0,
    },
  });

  const loadedEl = document.getElementById("loaded");
  const failedEl = document.getElementById("failed");
  const lastErrorEl = document.getElementById("last-error");
  let loaded = 0, failed = 0;

  const tileset = await Cesium.Cesium3DTileset.fromUrl("/tileset.json", {
    maximumScreenSpaceError: sse,
    debugShowBoundingVolume: debug,
  });
  viewer.scene.primitives.add(tileset);
  // Required ODbL attribution for OSM-derived building data, plus the
  // Protomaps planet build we range-read at upstream. Cesium also
  // auto-adds its own credit and the OpenStreetMap imagery credit.
  viewer.creditDisplay.addStaticCredit(
    new Cesium.Credit(
      'Building footprints © <a href="https://www.openstreetmap.org/copyright" target="_blank" rel="noopener">OpenStreetMap contributors</a> (ODbL)',
      true,
    ),
  );
  viewer.creditDisplay.addStaticCredit(
    new Cesium.Credit(
      'Vector tiles by <a href="https://protomaps.com/" target="_blank" rel="noopener">Protomaps</a>',
      true,
    ),
  );
  // Expose for debugging via DevTools console.
  window.viewer = viewer;
  window.tileset = tileset;
  window.Cesium = Cesium;

  tileset.tileLoad.addEventListener(() => {
    loaded++;
    loadedEl.textContent = String(loaded);
  });
  tileset.tileFailed.addEventListener((event) => {
    failed++;
    failedEl.textContent = String(failed);
    const url = event.url ?? "(unknown)";
    const msg = event.message ?? "(no message)";
    lastErrorEl.innerHTML = '<span class="err">' + url + '<br/>' + msg + '</span>';
    console.error("tile failed:", url, msg);
  });

  // ----- Cesium OSM Buildings comparison toggle -----
  // Lazy: only fetch Cesium OSM Buildings on first click. Falls back to
  // a "Cesium Ion needed" hint when the request 401s, so the dev page
  // still works without a configured token.
  const btnOurs = document.getElementById("btn-ours");
  const btnCesium = document.getElementById("btn-cesium");
  const toggleNote = document.getElementById("toggle-note");
  let cesiumOsm = null;
  let cesiumOsmLoading = null;

  function setActive(which) {
    btnOurs.classList.toggle("active", which === "ours");
    btnCesium.classList.toggle("active", which === "cesium");
    tileset.show = which === "ours";
    if (cesiumOsm) cesiumOsm.show = which === "cesium";
  }

  async function ensureCesiumOsm() {
    if (cesiumOsm) return cesiumOsm;
    if (cesiumOsmLoading) return cesiumOsmLoading;
    btnCesium.disabled = true;
    toggleNote.textContent = "loading Cesium OSM Buildings…";
    cesiumOsmLoading = Cesium.createOsmBuildingsAsync()
      .then((t) => {
        cesiumOsm = t;
        cesiumOsm.show = false;
        viewer.scene.primitives.add(cesiumOsm);
        toggleNote.textContent = "";
        return cesiumOsm;
      })
      .catch((err) => {
        console.error("Cesium OSM Buildings load failed", err);
        toggleNote.textContent = 'needs Ion token: ?ion=...';
        return null;
      })
      .finally(() => {
        btnCesium.disabled = false;
        cesiumOsmLoading = null;
      });
    return cesiumOsmLoading;
  }

  btnOurs.addEventListener("click", () => setActive("ours"));
  btnCesium.addEventListener("click", async () => {
    const loaded = await ensureCesiumOsm();
    if (loaded) setActive("cesium");
  });

  const panel = document.getElementById("panel");
  const panelTitle = document.getElementById("panel-title");
  const panelBody = document.getElementById("panel-body");
  const handler = new Cesium.ScreenSpaceEventHandler(viewer.canvas);
  handler.setInputAction((event) => {
    const picked = viewer.scene.pick(event.position);
    if (!(picked instanceof Cesium.Cesium3DTileFeature)) {
      panel.style.display = "none";
      return;
    }
    const ids = picked.getPropertyIds();
    panelBody.innerHTML = "";
    let title = "Building";
    for (const id of ids) {
      const v = picked.getProperty(id);
      if (id === "name" && v) title = v;
      const dt = document.createElement("dt"); dt.textContent = id;
      const dd = document.createElement("dd"); dd.textContent = String(v);
      panelBody.append(dt, dd);
    }
    panelTitle.textContent = title;
    panel.style.display = "block";
  }, Cesium.ScreenSpaceEventType.LEFT_CLICK);
</script>
</body>
</html>
`;

export const viewerHtml = (_c: Context) =>
  new Response(HTML, {
    headers: {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "public, max-age=300",
    },
  });
