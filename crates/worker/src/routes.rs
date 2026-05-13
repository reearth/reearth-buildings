use worker::*;

pub async fn root(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    Response::ok("reearth-buildings worker: see /tileset.json")
}

pub async fn healthz(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    Response::ok("ok")
}

/// Static tileset.json proxy. The actual JSON lives in the `STATIC` R2 bucket
/// under `buildings/tileset.json`; the worker just streams it through so we
/// can layer caching and CORS uniformly.
pub async fn tileset_json(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let bucket = ctx.env.bucket("STATIC")?;
    let Some(obj) = bucket.get("buildings/tileset.json").execute().await? else {
        return Response::error("tileset.json not found", 404);
    };
    let Some(body) = obj.body() else {
        return Response::error("tileset.json empty", 500);
    };
    let bytes = body.bytes().await?;
    let mut resp = Response::from_bytes(bytes)?;
    let headers = resp.headers_mut();
    headers.set("content-type", "application/json")?;
    headers.set("cache-control", "public, max-age=3600")?;
    headers.set("access-control-allow-origin", "*")?;
    Ok(resp)
}

/// `/{z}/{x}/{y}.glb` — placeholder until buildings-core renders real glb.
pub async fn glb_tile(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let (Some(z), Some(x), Some(y)) = (ctx.param("z"), ctx.param("x"), ctx.param("y")) else {
        return Response::error("Bad Request", 400);
    };
    let z: u8 = z.parse().map_err(|_| Error::from("invalid z"))?;
    let x: u32 = x.parse().map_err(|_| Error::from("invalid x"))?;
    let y: u32 = y.parse().map_err(|_| Error::from("invalid y"))?;

    if z != 14 {
        return Response::error("only z=14 is served in M1", 404);
    }

    let body = format!("TODO render glb for {z}/{x}/{y}");
    let mut resp = Response::ok(body)?;
    resp.headers_mut().set("content-type", "text/plain")?;
    Ok(resp)
}
