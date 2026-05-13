use worker::*;

mod routes;

#[event(fetch, respond_with_errors)]
pub async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    Router::new()
        .get_async("/", routes::root)
        .get_async("/healthz", routes::healthz)
        .get_async("/tileset.json", routes::tileset_json)
        .get_async("/:z/:x/:y.glb", routes::glb_tile)
        .run(req, env)
        .await
}
