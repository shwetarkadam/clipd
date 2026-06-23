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
 *
 * API: GET /stats
 *   → returns aggregated stats as JSON
 */

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const version = url.searchParams.get("v") || "unknown";
    const os = url.searchParams.get("os") || "unknown";
    const arch = url.searchParams.get("arch") || "unknown";
    const pathname = url.pathname;

    // /stats — aggregated stats (no params needed)
    if (pathname.endsWith("/stats")) {
      return handleStats(env);
    }

    // /count — read-only count lookup
    if (pathname.endsWith("/count")) {
      const key = `clipd:${version}:${os}:${arch}`;
      const n = (await env.PING_COUNT.get(key, "number")) || 0;
      return new Response(`${n}`, { headers: { "Content-Type": "text/plain" } });
    }

    // /ping — increment and return count
    const key = `clipd:${version}:${os}:${arch}`;
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

/**
 * Aggregate all clipd:* keys and return stats.
 */
async function handleStats(env) {
  const PREFIX = "clipd:";
  const totals = {
    total: 0,
    by_version: {},
    by_os: {},
    by_arch: {},
    by_os_arch: {},
  };

  // List all keys with clipd: prefix (Cloudflare KV list is eventually consistent)
  let cursor;
  do {
    const result = await env.PING_COUNT.list({
      prefix: PREFIX,
      cursor,
    });
    cursor = result.cursor;
    const keys = result.keys;

    for (const kv of keys) {
      const rawKey = kv.name;

      // Skip the total key — handle separately
      if (rawKey === "clipd:total") {
        totals.total = (await env.PING_COUNT.get("clipd:total", "number")) || 0;
        continue;
      }

      // Parse key: clipd:version:os:arch
      const parts = rawKey.split(":");
      if (parts.length !== 4 || parts[0] !== "clipd") continue;

      const [, ver, osKey, archKey] = parts;
      const count = (await env.PING_COUNT.get(rawKey, "number")) || 0;

      totals.by_version[ver] = (totals.by_version[ver] || 0) + count;
      totals.by_os[osKey] = (totals.by_os[osKey] || 0) + count;
      totals.by_arch[archKey] = (totals.by_arch[archKey] || 0) + count;

      const osArchKey = `${osKey}:${archKey}`;
      totals.by_os_arch[osArchKey] = (totals.by_os_arch[osArchKey] || 0) + count;
    }
  } while (cursor);

  return new Response(JSON.stringify(totals, null, 2), {
    headers: {
      "Content-Type": "application/json",
      "Cache-Control": "no-store",
    },
  });
}
