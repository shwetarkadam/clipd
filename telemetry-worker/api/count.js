/**
 * clipd telemetry counter — Vercel Edge Function + Upstash Redis
 *
 * Deploy with:
 *   vercel --prod
 *
 * Uses Upstash Redis (free tier: 10k commands/day).
 *   Sign up at https://upstash.com → Create Redis database
 *   Add KV_URL and KV_TOKEN to Vercel project env vars.
 *
 * API: GET /api/count?v=0.1.0&os=macos&arch=arm64
 *   → increments counter for version "0.1.0"
 *   → returns current count as plain text
 *
 * API: GET /api/count?v=0.1.0&os=macos&arch=arm64&read=1
 *   → returns current count without incrementing
 *
 * API: GET /api/stats
 *   → returns aggregated stats as JSON
 */

const PREFIX = "clipd:";

export const config = {
  runtime: "edge",
};

async function redis() {
  const { Redis } = await import("@upstash/redis");
  return new Redis({
    url: process.env.KV_URL,
    token: process.env.KV_TOKEN,
  });
}

export default async function handler(req) {
  const url = new URL(req.url);
  const pathname = url.pathname;

  // GET /api/stats
  if (pathname.endsWith("/stats")) {
    return handleStats();
  }

  const version = url.searchParams.get("v") || "unknown";
  const os = url.searchParams.get("os") || "unknown";
  const arch = url.searchParams.get("arch") || "unknown";
  const readOnly = url.searchParams.get("read") === "1";

  const key = `${PREFIX}${version}:${os}:${arch}`;

  try {
    const db = await redis();

    if (readOnly) {
      const n = (await db.get(key)) || 0;
      return new Response(`${n}`, {
        headers: { "Content-Type": "text/plain" },
      });
    }

    // Increment and get new value atomically
    const count = await db.incr(key);

    // Also increment total
    await db.incr(`${PREFIX}total`);

    return new Response(`${count}`, {
      headers: {
        "Content-Type": "text/plain",
        "Cache-Control": "no-store",
      },
    });
  } catch (e) {
    return new Response(`error: ${e.message}`, {
      status: 500,
      headers: { "Content-Type": "text/plain" },
    });
  }
}

async function handleStats() {
  try {
    const db = await redis();
    const totals = {
      total: (await db.get(`${PREFIX}total`)) || 0,
      by_version: {},
      by_os: {},
      by_arch: {},
      by_os_arch: {},
    };

    // Scan all clipd:* keys
    const allKeys = [];
    let cursor = "0";
    do {
      const [nextCursor, keys] = await db.scan(cursor, {
        match: `${PREFIX}*`,
        count: 100,
      });
      cursor = nextCursor;
      allKeys.push(...keys);
    } while (cursor !== "0");

    for (const rawKey of allKeys) {
      if (rawKey === `${PREFIX}total`) continue;

      const parts = rawKey.split(":");
      if (parts.length !== 4) continue;

      const [, ver, osKey, archKey] = parts;
      const count = (await db.get(rawKey)) || 0;

      totals.by_version[ver] = (totals.by_version[ver] || 0) + count;
      totals.by_os[osKey] = (totals.by_os[osKey] || 0) + count;
      totals.by_arch[archKey] = (totals.by_arch[archKey] || 0) + count;

      const osArchKey = `${osKey}:${archKey}`;
      totals.by_os_arch[osArchKey] = (totals.by_os_arch[osArchKey] || 0) + count;
    }

    return new Response(JSON.stringify(totals, null, 2), {
      headers: { "Content-Type": "application/json" },
    });
  } catch (e) {
    return new Response(`error: ${e.message}`, {
      status: 500,
      headers: { "Content-Type": "text/plain" },
    });
  }
}
