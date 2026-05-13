import type { Context } from "hono";
import type { Env } from "../env";

export const tilesetJson = async (c: Context<{ Bindings: Env }>) => {
  const obj = await c.env.STATIC.get("buildings/tileset.json");
  if (!obj) return c.text("tileset.json not found", 404);
  return new Response(obj.body, {
    headers: {
      "content-type": "application/json",
      "access-control-allow-origin": "*",
    },
  });
};
