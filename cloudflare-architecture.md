# Cloudflare Workers アーキテクチャ方針

Re:Earth Terrain / Re:Earth Buildings の配信基盤を **Cloudflare Workers + R2** 上に構築する際の技術アーキテクチャ方針をまとめる。

- 想定読者：本プロジェクトを実装・運用するエンジニア
- 関連ドキュメント：[Re:Earthデータ配信戦略（企画書）](https://www.notion.so/eukarya/Re-Earth-35f16e0fb165800c8744d1291dc1f19a)

> **2026-05 更新**：データソースを Protomaps OSM PMTiles → **Overture Maps Buildings PMTiles** に置換。地面標高は EGM2008 の自前 COG → **Re:Earth Terrain（terrain.reearth.land/terrarium/ellipsoid/{z}/{x}/{y}.webp）** をオンデマンド取得して建物中心点でサンプル。これに伴い R2 のジオイド COG と `egm.ts`/`cog.ts` は廃止。以下の本文には旧記述（Protomaps / EGM）が残っている箇所があるが、考え方は同じ（外部公開エンドポイントを Worker から直参照）。

---

## 1. 設計方針

### 1.1 なぜ Cloudflare Workers に振り切るのか

| 項目 | Workers Only | VPS + R2 | VPS Only |
|---|---|---|---|
| エグレスコスト | 0（R2無料） | 0（R2無料） | 帯域に応じて課金 |
| エッジキャッシュ | 標準で全世界300+拠点 | 別途CDN必要 | 同左 |
| 運用工数 | デプロイのみ | OS/プロセス管理 | 同左 |
| スケール | 自動・無限 | 手動 | 手動 |
| コールドスタート | 数ms（Isolate） | 常時起動 | 常時起動 |
| CPU/メモリ制約 | **30s / 128MB** | 自由 | 自由 |

→ **コスト・スケール・運用工数で圧倒的優位**。CPU/メモリ制約はタイル単位処理なら十分収まる。

### 1.2 採用技術スタック

| レイヤ | 技術 | 理由 |
|---|---|---|
| ランタイム | Cloudflare Workers (workerd) | エッジ実行・自動スケール |
| 言語 | **Rust → WASM** | 既存 stralift コードベース流用、高速 |
| SDK | [`workers-rs`](https://github.com/cloudflare/workers-rs) | 公式Rustバインディング |
| ストレージ | Cloudflare R2 | エグレス無料、S3互換 |
| キャッシュ | Cache API + R2 二層 | エッジ高速＋永続 |
| メタデータ | Workers KV | tilejson/layer.json 等の軽量データ |
| ルーティング | Workers Routes | `terrain.reearth.land` / `buildings.reearth.land` |
| ビルド | wasm-pack + wrangler | 標準ツールチェーン |
| 監視 | Workers Analytics + Tail Logs | 標準機能で十分 |

### 1.3 設計原則

1. **バッチ処理ゼロ運用**：事前タイル化・定期同期ジョブは一切持たない。すべてオンデマンドでリクエスト時に生成
2. **タイル単位で決定論的生成**：同じ入力→同じ出力。キャッシュ最大化
3. **PMTiles は外部公開エンドポイントを直参照**：Overture の `overturemaps-tiles-us-west-2-beta.s3.amazonaws.com/{RELEASE}/buildings.pmtiles` を Worker から HTTP Range で読む。自前でミラーしない
4. **地面標高も外部直参照**：Re:Earth Terrain の Terrarium WebP を出力タイルと同じ (z, x, y) で fetch、WASM 内で建物中心点を bilinear サンプル。R2 にジオイドや DEM を持たない
5. **書き込みは生成キャッシュのみ**：R2 への書き込みはレンダリング結果のキャッシュ用途のみ
6. **Worker は薄く、計算は WASM に集中**：JavaScript 層は最小化
7. **失敗時は503で正直に**：30sタイムアウトに当たったらリトライさせる

---

## 2. システム全体図

```
                  ┌─────────────────────────────────────────────┐
                  │     Cloudflare Edge Network (300+ PoPs)     │
                  │                                              │
   Client ──────► │  ┌──────────────────────────────────────┐   │
                  │  │  Cache API (edge cache, free, ~ms)    │   │
                  │  └────────┬──────────────────────────────┘   │
                  │           │ MISS                              │
                  │           ▼                                   │
                  │  ┌──────────────────────────────────────┐    │
                  │  │  Worker (Rust→WASM)                  │    │
                  │  │   ├ Routing                          │    │
                  │  │   ├ Param validation                 │    │
                  │  │   ├ Cache key derivation             │    │
                  │  │   ├ R2 generated-cache lookup        │◄───┼──── R2 GET
                  │  │   │  └ HIT → return (& warm Cache API)│   │
                  │  │   │  └ MISS:                          │   │
                  │  │   │     ├ Fetch source from R2       │◄──┼──── R2 GET (range)
                  │  │   │     ├ Decode & process           │    │
                  │  │   │     ├ Encode output (glb/png/...)│    │
                  │  │   │     ├ Write to R2 cache (async)  │───┼──► R2 PUT
                  │  │   │     └ Return                      │   │
                  │  └──────────────────────────────────────┘    │
                  │                                              │
                  └─────────────────────────────────────────────┘
                              │            ▲
                              ▼            │
                  ┌─────────────────────────────────────────────┐
                  │           Cloudflare R2 (Storage)            │
                  │                                              │
                  │  /sources/   (一度アップロードしたら触らない)  │
                  │   ├ dem/{provider}/{tile.tif}                │
                  │   └ geoid/egm2008.tif                        │
                  │                                              │
                  │  /cache/   (オンデマンド生成結果を書き戻し)   │
                  │   ├ terrain/{encoding}/{z}/{x}/{y}.{fmt}     │
                  │   └ buildings/{z}/{x}/{y}.glb                │
                  │                                              │
                  │  /static/                                    │
                  │   ├ terrain/tilejson.json                    │
                  │   └ buildings/tileset.json                   │
                  └─────────────────────────────────────────────┘

                  ┌─────────────────────────────────────────────┐
                  │  External (直参照、自前ミラーなし)            │
                  │   build.protomaps.com/{YYYYMMDD}.pmtiles     │
                  │   ↑ Worker が HTTP Range Request で直読み    │
                  └─────────────────────────────────────────────┘
```

---

## 3. サービス別アーキテクチャ

### 3.1 Re:Earth Terrain

**URL**: `terrain.reearth.land`
**役割**: DEM + ジオイド → 楕円体高タイル

#### エンドポイント

| パス | 説明 |
|---|---|
| `/{encoding}/{data_type}/{z}/{x}/{y}.{format}` | タイル取得 |
| `/{encoding}/{data_type}/tilejson.json` | TileJSON 3.0.0 |
| `/cesium/{data_type}/layer.json` | Cesium heightmap layer |
| `/cesium/{data_type}/{z}/{x}/{y}.terrain` | Cesium heightmap タイル |
| `/cesium-mesh/{data_type}/layer.json` | Cesium quantized-mesh layer |
| `/cesium-mesh/{data_type}/{z}/{x}/{y}.terrain` | Cesium quantized-mesh タイル |

- `encoding` ∈ {`terrarium`, `mapbox`}
- `data_type` ∈ {`geoid`, `elevation`, `ellipsoid`}
- `format` ∈ {`webp`, `png`, `avif`}

#### 処理フロー（タイルリクエスト）

```
Request
  ↓ Worker
  ↓ Cache API lookup
  ↓ MISS → R2 cache lookup (/cache/terrain/...)
  ↓ MISS → 
    1. R2 から該当 DEM COG タイルを HTTP Range で取得
    2. R2 から該当ジオイドCOGタイルを取得
    3. 双方をブレンド（既存 blend ロジック）
    4. ellipsoid = elevation + geoid を計算
    5. 指定 encoding で RGB 化
    6. WebP/PNG/AVIF にエンコード
  ↓ R2 へ非同期書き込み（ctx.waitUntil）
  ↓ Cache API に格納
  ↓ Response
```

#### Cesium quantized-mesh 生成の特殊事項

- Martini RTIN（既存 `martini` crate）を WASM 化
- 出力は gzip 圧縮済み `.terrain`
- メッシュ単純化により出力サイズを抑える（タイル平均 5-50KB）

### 3.2 Re:Earth Buildings

**URL**: `buildings.reearth.land`
**役割**: OSM建物 → 3D Tiles 1.1 (glb) オンデマンド生成

#### エンドポイント

| パス | 説明 |
|---|---|
| `/tileset.json` | 3D Tiles 1.1 ルート tileset |
| `/{z}/{x}/{y}.glb` | 建物 glb タイル |

`tileset.json` は静的 JSON として R2 に置き、Worker は単に proxy。

#### tileset.json 構造

```json
{
  "asset": {
    "version": "1.1",
    "copyright": "© OpenStreetMap contributors"
  },
  "geometricError": 500,
  "root": {
    "boundingVolume": { "region": [...world bounds...] },
    "geometricError": 500,
    "refine": "REPLACE",
    "content": null,
    "children": [
      // implicit tiling subtree at z=0..15
    ]
  },
  "extensionsUsed": ["3DTILES_implicit_tiling"]
}
```

- **Implicit Tiling** を使用してタイル単位URLを `{z}/{x}/{y}.glb` に直接マッピング
- Cesium / MapLibre / three.js すべてで読める

#### 処理フロー（glb タイルリクエスト）

```
GET /14/14552/6451.glb
  ↓ Worker
  ↓ Cache API lookup
  ↓ MISS → R2 cache lookup (/cache/buildings/14/14552/6451.glb)
  ↓ MISS → 
    1. PMTiles ヘッダ＋ディレクトリを KV から取得（事前ロード済み）
    2. R2 から対応する MVT タイルを HTTP Range で取得
    3. gzip 解凍
    4. MVT デコード → buildings レイヤー抽出
    5. 各 feature: ポリゴン + height/min_height 属性
    6. ポリゴンを earcut で三角形分割
    7. height で押し出し、側面メッシュ生成
    8. 全建物を1メッシュにマージ（vertex/index 圧縮）
    9. ECEF or Local ENU 座標系へ変換
    10. glb（glTF 2.0 binary）にシリアライズ
    11. オプション：meshopt 圧縮拡張 (KHR_mesh_quantization, EXT_meshopt_compression)
  ↓ R2 へ非同期書き込み
  ↓ Cache API に格納
  ↓ Response
```

#### 高さの決定ルール

1. `height` タグがあればそれを使用（メートル）
2. なければ `building:levels` × 3.0m
3. それもなければデフォルト 6.0m（2階建て相当）
4. `min_height` があれば底面オフセット

#### LOD 設計

| Zoom | 内容 | 平均glb サイズ目安 |
|---|---|---|
| 14 | 個別建物すべて | 50-300 KB |
| 13 | 大規模建物のみ（footprint面積>閾値） | 30-100 KB |
| 12 | 簡略化された街区メッシュ | 10-30 KB |
| ≤11 | 配信せず（geometricError で省略） | - |

初期実装は **z=14 のみ**。LOD は Phase 2 で実装。

---

## 4. データ取り扱い（バッチ処理なし）

### 4.1 ソースデータの所在

| ソース | 所在 | 取り扱い |
|---|---|---|
| 建物 PMTiles | **`overturemaps-tiles-us-west-2-beta.s3.amazonaws.com/{RELEASE}/buildings.pmtiles`（Overture公開）** | Worker から HTTP Range で直接読む。自前ミラーしない。月次リリースを S3 ListBucket で自動探索 |
| 地面標高 | **`terrain.reearth.land/terrarium/ellipsoid/{z}/{x}/{y}.webp`（Re:Earth Terrain）** | 出力タイルと同じ (z, x, y) を fetch、WASM 内で WebP デコード→建物中心 bilinear サンプル。値は楕円体高（EGM2008 を blend 済み） |

→ **定期同期ジョブ・cron・GitHub Actions ワークフロー一切不要**。

### 4.2 PMTiles ディレクトリの取り扱い

PMTiles の root directory は数MB。**毎リクエストで読み出すのは無駄**だが、Cloudflareの仕組みで簡潔に解決：

- 起動時（Worker初期化時）に Protomaps から root directory を Range Request で取得
- Workers KV に key=`pmtiles:root:{version}` で保存（TTL 数日）
- 以降の Worker 呼び出しは KV から読む（読み取り <10ms）
- リーフディレクトリは LRU キャッシュ（Worker 内静的変数）

### 4.3 PMTiles のバージョン切り替え

- 環境変数 `PMTILES_VERSION` に日付（例 `20260513`）を持つ
- 新バージョンへの切り替えは **wrangler deploy で環境変数を更新するだけ**（バッチジョブ不要）
- R2 上の `/cache/buildings/` は**プレフィックスに version を含める**ことで暗黙的に無効化

```
/cache/buildings/v20260513/14/14552/6451.glb
/cache/buildings/v20260514/14/14552/6451.glb  ← 新バージョン
```

旧バージョンは R2 Lifecycle Rule で TTL 自動削除（管理コード書かない）。

---

## 5. キャッシュ戦略

### 5.1 三層キャッシュ

| 層 | 媒体 | TTL | 速度 | 役割 |
|---|---|---|---|---|
| L1 | Cloudflare Edge Cache (Cache API) | 30日 | <10ms | 同一PoPでの再リクエスト |
| L2 | R2（`/cache/...`） | 永続（明示削除まで） | 30-100ms | エッジ退避後の再生成回避 |
| L3 | （再計算） | - | 200ms-2s | 最終手段 |

### 5.2 Cache API キー設計

```
https://terrain.reearth.land/terrarium/ellipsoid/14/14552/6451.webp
                                       ↑ パス全体が cache key（Cache API デフォルト）
```

- クエリパラメータは `Vary` で扱う
- 個別ユーザー固有のヘッダは無視（ステートレス配信）

### 5.3 Cache-Control / ETag

| リソース | Cache-Control | ETag |
|---|---|---|
| タイル本体 | `public, max-age=2592000, immutable`（30日） | ソース版数+座標 |
| tilejson / layer.json / tileset.json | `public, max-age=3600`（1時間） | ファイルハッシュ |

`immutable` 付けるのは**バージョン付きURL前提**。`?v=20260513` をクライアントに付けてもらうか、Workers側でリダイレクトする方式を検討。

### 5.4 R2 キャッシュ書き込み

```rust
// 同期で返却し、書き込みはバックグラウンドで
let response = build_response(glb);
ctx.wait_until(async {
    r2.put(cache_key, glb_bytes).await.ok();
});
response
```

→ レスポンス遅延に影響しない。失敗してもユーザーには見えない（次回再生成）。

---

## 6. Worker コード構成

### 6.1 リポジトリ構成（**2 リポジトリ分離**）

データ源・処理ロジック・依存ライブラリの重複が少ないため、**Terrain と Buildings は別リポジトリ**として運用する。CI / リリース / 責任者 / スポンサー受け入れを独立させやすく、貢献者も呼びやすい。

```
reearth-terrain/         ★ 新規リポジトリ
  crates/
    async-cog/             既存 stralift から移管 or 共通 crate として publish
    terrain-core/          DEM 合成・ジオイド統合・タイル生成（既存 stralift から抽出）
    martini/               既存から移管
    quantized-mesh/        既存から移管
    worker/                terrain.reearth.land の Cloudflare Worker

reearth-buildings/       ★ 新規リポジトリ
  crates/
    pmtiles-reader/        PMTiles range-request リーダー（WASM対応）
    mvt-decoder/           MVT デコード
    buildings-core/        押し出し・S3DB解釈・glb 生成
    worker/                buildings.reearth.land の Cloudflare Worker
```

既存 `stralift` リポジトリのうち、Terrain 系コアは `reearth-terrain` に移管。`async-cog` は両者で使い回す可能性があるなら `crates.io` 公開 crate として独立させるのが筋。

### 6.2 worker-terrain の擬似コード構成

```rust
// crates/worker-terrain/src/lib.rs
use worker::*;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    Router::new()
        .get_async("/:enc/:dt/:z/:x/:y.:fmt", tile_handler)
        .get_async("/:enc/:dt/tilejson.json", tilejson_handler)
        .get_async("/cesium-mesh/:dt/:z/:x/:y.terrain", quantized_mesh_handler)
        .run(req, env).await
}

async fn tile_handler(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let cache_key = req.url()?.to_string();
    
    // L1: Cache API
    if let Some(resp) = Cache::default().get(&cache_key, false).await? {
        return Ok(resp);
    }
    
    let (enc, dt, z, x, y, fmt) = parse_params(&ctx)?;
    let r2 = ctx.env.bucket("DATA")?;
    
    // L2: R2 generated cache
    let r2_key = format!("cache/terrain/{enc}/{dt}/{z}/{x}/{y}.{fmt}");
    if let Some(obj) = r2.get(&r2_key).execute().await? {
        let resp = build_tile_response(obj.body()?, fmt)?;
        Cache::default().put(&cache_key, resp.cloned()?).await?;
        return Ok(resp);
    }
    
    // L3: Generate
    let bytes = stralift_terrain::render_tile(&r2, dt, z, x, y, enc, fmt).await?;
    let resp = build_tile_response(&bytes, fmt)?;
    
    // Write back caches
    let r2_clone = r2.clone();
    let bytes_clone = bytes.clone();
    ctx_wait_until(async move {
        r2_clone.put(&r2_key, bytes_clone).execute().await.ok();
    });
    Cache::default().put(&cache_key, resp.cloned()?).await?;
    
    Ok(resp)
}
```

### 6.3 既存 stralift コードの WASM 対応で気をつけること

| 既存依存 | WASM対応状況 | 対応 |
|---|---|---|
| `tokio` | △ ランタイム不要なら回避 | `workers-rs` のexecutor使用 |
| `reqwest` | × | Worker の `Fetch` API or R2 binding使用 |
| `image` (webp/png/avif encode) | ○ | pure Rust feature を有効化 |
| `flate2` (gzip) | ○ | `pure-rust` feature を有効化 |
| `async-tiff` | △ I/O部分要差し替え | R2 + 自前 Reader 実装 |
| `pmtiles` | △ 同上 | R2 + 自前 Reader 実装 |
| `geo` / `geo-types` | ○ | そのまま |
| `earcutr` | ○ | そのまま |

→ **I/O 層を抽象化し、ネイティブ版（既存サーバ用）と Workers 版を切り替えられるようにする**のが鍵。

```rust
// 既存
pub trait ByteReader: Send + Sync {
    async fn read_range(&self, start: u64, end: u64) -> Result<Bytes>;
}

// ネイティブ: HTTP client / fs
// Workers: R2 binding
```

---

## 7. Workers 制約と回避策

### 7.1 CPU 時間 30 秒

- **タイル単位**なら 30s は余裕（実測：z=14 建物タイル glb 生成 ~100-500ms 想定）
- 万一近づいたら：
  - 並列処理は Worker 内では限定的、別 Worker への RPC で分散
  - LOD で建物数を制限

### 7.2 メモリ 128 MB

- 1タイル処理に必要なメモリ：
  - PMTiles range: ~150KB
  - 解凍後 MVT: ~300KB
  - 中間ジオメトリ: 数MB
  - glb 出力: ~300KB
- → 余裕、ただし**大規模都市（z=14 NY/東京）でメッシュ統合時にピーク注意**

### 7.3 リクエストボディ / レスポンス

- 出力 glb は数百KB〜数MB。問題なし
- 入力は単純な GET。問題なし

### 7.4 バンドルサイズ 10 MB（圧縮後）

- Rust→WASM は意外と大きい。**wasm-opt + 不要feature削除** で削る
- 厳しければ Worker を機能ごと分割（terrain / buildings 別 Worker は既にそう）

### 7.5 Sub-request 上限

- 無料プラン 50、Paid 1000 sub-requests/req
- 通常は 1 タイルあたり R2 access 2-5回。十分

---

## 8. デプロイ & 環境

### 8.1 環境分離

| 環境 | URL | 用途 |
|---|---|---|
| dev | `dev-terrain.reearth.land` / `dev-buildings.reearth.land` | 開発 |
| staging | `staging-*.reearth.land` | パートナーβ |
| prod | `terrain.reearth.land` / `buildings.reearth.land` | 本番 |

すべて wrangler.toml の environments で切り替え。

### 8.2 R2 バケット構成

| バケット | 用途 |
|---|---|
| `reearth-sources` | ソースデータ（読み取り専用） |
| `reearth-cache-prod` | 本番生成キャッシュ |
| `reearth-cache-staging` | staging |
| `reearth-static` | tileset.json / tilejson 等の静的ファイル |

### 8.3 CI/CD

- GitHub Actions で：
  - PR時: cargo test, wasm-pack build, wrangler dry-run
  - main merge時: dev環境へ自動デプロイ
  - tag (v*) push時: staging → 手動承認 → prod

---

## 9. 観測・運用

### 9.1 メトリクス（Workers Analytics）

標準で取れる：
- リクエスト数（PoP別・国別）
- レスポンスタイム（p50/p95/p99）
- エラー率
- CPU時間分布

カスタム：
- `console.log` で構造化ログ → Tail で見る
- 重要メトリクスは Analytics Engine に記録

### 9.2 アラート

- エラー率 >1% で Slack 通知
- p95 レスポンスタイム >2s で同上
- R2 ストレージ使用量が予算超で通知

### 9.3 ダッシュボード

Cloudflare Dashboard 標準 + 必要に応じて Grafana（Analytics Engine SQL）。

---

## 10. コストガードレール

### 10.1 必須設定

| 項目 | 値 | 場所 |
|---|---|---|
| **Workers Spend Limit** | $500 / 月 | Cloudflare Account |
| Workers Rate Limit | 1000 req/min per IP | wrangler.toml |
| WAF カスタムルール | Bot Fight Mode ON | Cloudflare Dashboard |
| R2 書き込みレート上限 | Worker 内で 100 ops/sec | コード制御 |

### 10.2 異常検知

- 単一IPから 10分で1万req → 自動ブロック
- 同一 cache key が連続100回 MISS → 警告（生成バグの疑い）

---

## 11. 段階的実装計画

### M1: スケルトン（〜2週間）

- [ ] `worker-terrain` の枠組み（ハローワールド + R2 binding）
- [ ] `worker-buildings` の枠組み
- [ ] wrangler 設定（dev環境）
- [ ] R2 バケット作成、サンプルデータ配置

### M2: Terrain MVP（〜4週間）

- [ ] DEM/Geoid COG を R2 から読み出す Reader 実装
- [ ] 既存 stralift コアロジックの WASM 対応（I/O 抽象化）
- [ ] Terrarium WebP タイル生成
- [ ] tilejson.json 配信
- [ ] L1/L2 キャッシュ実装

### M3: Buildings MVP（〜6週間）

- [ ] PMTiles リーダー WASM 対応
- [ ] MVT デコード
- [ ] 建物押し出し（earcut + extrusion）
- [ ] glb シリアライザ
- [ ] tileset.json 配信
- [ ] L1/L2 キャッシュ実装

### M4: Cesium 対応（〜2週間）

- [ ] quantized-mesh エンドポイント
- [ ] heightmap-1.0 エンドポイント
- [ ] Cesium ビューアでの動作確認

### M5: 本番化（〜2週間）

- [ ] staging → prod デプロイパイプライン
- [ ] WAF / Rate Limit 設定
- [ ] Spend Limit 設定
- [ ] 監視・アラート
- [ ] ライセンス遵守チェック（ODbL attribution 等）

---

## 12. オープン問題

- **Implicit Tiling vs Explicit**：Cesium が Implicit Tiling 完全対応か、Explicit でフォールバック必要か要検証
- **glb の座標系**：ECEF 絶対座標 vs タイルローカル + transform。クライアント実装を確認
- **MVT 簡略化の影響**：Protomaps の z=14 building は OSM 原本からどの程度簡略化されているか
- **R2 → Worker のレイテンシ**：同リージョン内で実測必要（数十ms想定だが要確認）
- **OSM 編集即時反映**：Protomaps daily build までの遅延（最大1日）を許容するか、自前で planetiler 運用するか
- **wrangler.toml と Cargo.toml の整合**：workers-rs のバージョン追従ポリシー

---

## 参考リンク

- [Cloudflare Workers Docs](https://developers.cloudflare.com/workers/)
- [workers-rs](https://github.com/cloudflare/workers-rs)
- [Cloudflare R2 Docs](https://developers.cloudflare.com/r2/)
- [Protomaps Cloudflare Integration](https://docs.protomaps.com/deploy/cloudflare)
- [3D Tiles 1.1 Spec](https://github.com/CesiumGS/3d-tiles)
- [PMTiles Spec](https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md)
