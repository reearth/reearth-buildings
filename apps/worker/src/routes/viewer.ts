import type { Context } from "hono";

// Minimal Cesium viewer for dogfooding. Loads our /tileset.json on top of
// the OpenStreetMap basemap (no Ion token required). Camera initial state
// is overridable via query params:
//   /?lon=139.7&lat=35.68&height=2000&heading=0&pitch=-30
//
// Building click reveals attributes via a tiny side panel.
//
// The HTML is intentionally a single embedded string so the worker
// doesn't need an asset binding to serve it.

const HTML = `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>Re:Earth Buildings — viewer</title>
<meta name="viewport" content="width=device-width,initial-scale=1" />
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/cesium@1.123/Build/Cesium/Widgets/widgets.css" />
<style>
  html, body, #app { width: 100%; height: 100%; margin: 0; padding: 0; overflow: hidden; }
  #panel {
    position: absolute; top: 12px; right: 12px; max-width: 320px;
    background: rgba(20, 22, 28, 0.92); color: #fff; padding: 12px 14px;
    border-radius: 8px; font: 13px/1.45 -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
    box-shadow: 0 4px 20px rgba(0,0,0,0.3); display: none;
  }
  #panel h2 { margin: 0 0 6px; font-size: 13px; font-weight: 600; color: #9fd1ff; }
  #panel dl { margin: 0; display: grid; grid-template-columns: max-content 1fr; gap: 2px 10px; }
  #panel dt { color: #8b95a4; }
  #panel dd { margin: 0; word-break: break-all; }
  #hint {
    position: absolute; bottom: 12px; left: 12px;
    background: rgba(20, 22, 28, 0.85); color: #cfd6e0; padding: 6px 10px;
    border-radius: 6px; font: 12px -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
    pointer-events: none;
  }
</style>
</head>
<body>
<div id="app"></div>
<div id="panel"><h2 id="panel-title">Building</h2><dl id="panel-body"></dl></div>
<div id="hint">click a building to inspect its properties</div>
<script type="module">
  import * as Cesium from "https://cdn.jsdelivr.net/npm/cesium@1.123/Build/Cesium/index.js";

  // Surface the asset bundle path Cesium needs for its own static files.
  window.CESIUM_BASE_URL = "https://cdn.jsdelivr.net/npm/cesium@1.123/Build/Cesium/";

  const params = new URLSearchParams(location.search);
  const lon = Number(params.get("lon") ?? 139.7649);
  const lat = Number(params.get("lat") ?? 35.6785);
  const height = Number(params.get("height") ?? 1500);
  const heading = Number(params.get("heading") ?? 0);
  const pitch = Number(params.get("pitch") ?? -30);

  const viewer = new Cesium.Viewer("app", {
    baseLayer: Cesium.ImageryLayer.fromProviderAsync(
      Promise.resolve(new Cesium.OpenStreetMapImageryProvider({ url: "https://tile.openstreetmap.org/" })),
    ),
    baseLayerPicker: false,
    geocoder: false,
    homeButton: false,
    sceneModePicker: false,
    navigationHelpButton: false,
    timeline: false,
    animation: false,
    fullscreenButton: false,
  });
  viewer.scene.globe.depthTestAgainstTerrain = false;
  viewer.camera.setView({
    destination: Cesium.Cartesian3.fromDegrees(lon, lat, height),
    orientation: {
      heading: Cesium.Math.toRadians(heading),
      pitch: Cesium.Math.toRadians(pitch),
      roll: 0,
    },
  });

  const tileset = await Cesium.Cesium3DTileset.fromUrl("/tileset.json", {
    maximumScreenSpaceError: 16,
    skipLevelOfDetail: true,
  });
  viewer.scene.primitives.add(tileset);

  // Feature picking.
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
