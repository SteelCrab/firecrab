import { contentTypeFor, isPublishedRegistryObject, registryObjectKey } from "./registry";

const HEALTH_PATH = "/_health";

function responseForObject(object: R2ObjectBody, request: Request): Response {
  const headers = new Headers();
  object.writeHttpMetadata(headers);
  headers.set("content-type", headers.get("content-type") ?? contentTypeFor(object.key));
  headers.set("content-length", String(object.size));
  headers.set("etag", object.httpEtag);
  headers.set("x-content-type-options", "nosniff");
  headers.set("cache-control", headers.get("cache-control") ?? "no-cache");

  return new Response(request.method === "HEAD" ? null : object.body, { headers });
}

function methodNotAllowed(): Response {
  return new Response("Method not allowed\n", {
    status: 405,
    headers: { allow: "GET, HEAD" },
  });
}

function healthResponse(request: Request): Response {
  return new Response(
    request.method === "HEAD"
      ? null
      : JSON.stringify({ service: "firecrab-microregistry", status: "ok" }),
    {
      headers: {
        "cache-control": "no-store",
        "content-type": "application/json; charset=utf-8",
      },
    },
  );
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    if (request.method !== "GET" && request.method !== "HEAD") {
      return methodNotAllowed();
    }

    const url = new URL(request.url);
    if (url.pathname === HEALTH_PATH) return healthResponse(request);

    const key = registryObjectKey(url.pathname);
    if (key === null || !isPublishedRegistryObject(key)) {
      return env.ASSETS.fetch(request);
    }

    try {
      const object = await env.REGISTRY.get(key);
      if (object === null) return new Response("Not found\n", { status: 404 });

      return responseForObject(object, request);
    } catch (error) {
      console.error(
        JSON.stringify({
          event: "microregistry.object_read_failed",
          key,
          error: error instanceof Error ? error.message : "unknown error",
        }),
      );
      return new Response("Registry unavailable\n", { status: 503 });
    }
  },
} satisfies ExportedHandler<Env>;
