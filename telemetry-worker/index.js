/**
 * clipd telemetry counter — Cloudflare Worker
 *
 * Deploy with:
 *   npm create cloudflare@latest -- --type worker --name clipd-telemetry
 *   wranglers.toml: kv_namespaces = [{binding = "PING_COUNT", id = "<your-kv-namespace-id>"}]
 *
 * Or use Cloudflare Dashboard → Workers & Pages → Create → Edit inline.
 *
 * API: GET /ping?v=0.1.2&os=macos&arch=arm64
 *   → increments counter for version "0.1.2"
 *   → returns current count as plain text
 *
 * API: GET /count?v=0.1.2
 *   → returns current count for version without incrementing
 */

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const version = url.searchParams.get("v") || "unknown";
    const os = url.searchParams.get("os") || "unknown";
    const arch = url.searchParams.get("arch") || "unknown";
    const action = url.pathname.endsWith("/count") ? "count" : "ping";

    const key = `clipd:${version}:${os}:${arch}`;

    if (action === "count") {
      const n = (await env.PING_COUNT.get(key, "number")) || 0;
      return new Response(`${n}`, { headers: { "Content-Type": "text/plain" } });
    }

    // ping — increment
    const current = (await env.PING_COUNT.get(key, "number")) || 0;
    await env.PING_COUNT.put(key, String(current + 1));

    // Also keep a total
    const totalKey = `clipd:total`;
    const total = (await env.PING_COUNT.get(totalKey, "number")) || 0;
    await env.PING_COUNT.put(totalKey, String(total + 1));

    return new Response(`${current + 1}`, {
      headers: {
        "Content-Type": "text/plain",
        "Cache-Control": "no-store",
      },
    });
  },
};
