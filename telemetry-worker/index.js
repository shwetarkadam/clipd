/**
 * clipd telemetry counter — Cloudflare Worker
 *
 * Counts two different things, deliberately kept apart:
 *
 *   starts  — how often a daemon booted. The original counter. Useful for
 *             spotting crash loops, useless for "how many people use this":
 *             one person restarting fifty times looks like fifty users.
 *   people  — how many distinct installs exist, and how many were seen today
 *             and this month. This is what the install id is for. The client
 *             has always sent it; the worker used to throw it away.
 *
 * Privacy: the id is a random UUID the client generates for itself. No IP, no
 * user agent, no clipboard content is stored, and nothing here can be traced
 * back to a person. The per-day and per-month markers expire on their own —
 * only the anonymous install record and the counters outlive them.
 *
 * KV is eventually consistent and these read-modify-writes are not atomic, so
 * two daemons starting in the same instant can lose a tick. At this scale that
 * is noise, and undercounting is the safe direction for a number you are going
 * to quote to somebody.
 *
 * API
 *   GET /ping?v=0.4.14&os=macos&arch=aarch64&id=<uuid>  → record, return starts
 *   GET /count?v=..&os=..&arch=..                       → starts, no write
 *   GET /stats                                          → JSON, everything
 */

const DAY_TTL = 60 * 60 * 48; // 48h — covers a day plus clock skew
const MONTH_TTL = 60 * 60 * 24 * 40; // 40d

const utcDay = (d = new Date()) => d.toISOString().slice(0, 10);
const utcMonth = (d = new Date()) => d.toISOString().slice(0, 7);

async function bump(env, key) {
  const n = (await env.PING_COUNT.get(key, "number")) || 0;
  await env.PING_COUNT.put(key, String(n + 1));
  return n + 1;
}

/// Record this install once per bucket. Returns nothing; the counters move
/// only the first time an id is seen in that bucket.
async function recordPerson(env, id, version, os, arch) {
  if (!id) return;

  // Unique installs, ever. The value is the first sighting, which makes
  // "installs that never came back" answerable later without another key.
  if ((await env.PING_COUNT.get(`install:${id}`)) === null) {
    await env.PING_COUNT.put(
      `install:${id}`,
      JSON.stringify({ v: version, os, arch, first: new Date().toISOString() })
    );
    await bump(env, "installs:total");
  }

  const day = utcDay();
  if ((await env.PING_COUNT.get(`day:${day}:${id}`)) === null) {
    await env.PING_COUNT.put(`day:${day}:${id}`, "1", { expirationTtl: DAY_TTL });
    await bump(env, `dau:${day}`);
  }

  const month = utcMonth();
  if ((await env.PING_COUNT.get(`month:${month}:${id}`)) === null) {
    await env.PING_COUNT.put(`month:${month}:${id}`, "1", { expirationTtl: MONTH_TTL });
    await bump(env, `mau:${month}`);
  }
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const version = url.searchParams.get("v") || "unknown";
    const os = url.searchParams.get("os") || "unknown";
    const arch = url.searchParams.get("arch") || "unknown";
    const id = url.searchParams.get("id") || "";
    const pathname = url.pathname;

    if (pathname.endsWith("/stats")) return handleStats(env);

    if (pathname.endsWith("/count")) {
      const n = (await env.PING_COUNT.get(`clipd:${version}:${os}:${arch}`, "number")) || 0;
      return new Response(`${n}`, { headers: { "Content-Type": "text/plain" } });
    }

    // starts — unchanged, so the existing history stays comparable
    const starts = await bump(env, `clipd:${version}:${os}:${arch}`);
    await bump(env, "clipd:total");

    // people — the part that was missing
    await recordPerson(env, id, version, os, arch);

    return new Response(`${starts}`, {
      headers: { "Content-Type": "text/plain", "Cache-Control": "no-store" },
    });
  },
};

/// Everything, aggregated. Starts and people are reported separately and
/// labelled, because they are easy to confuse and differ by an order of
/// magnitude.
async function handleStats(env) {
  const out = {
    people: { installs_total: 0, active_today: 0, active_this_month: 0, daily: {}, monthly: {} },
    starts: { total: 0, by_version: {}, by_os: {}, by_arch: {}, by_os_arch: {} },
    note: "starts counts daemon launches; people counts distinct installs.",
  };

  out.people.installs_total = (await env.PING_COUNT.get("installs:total", "number")) || 0;

  // Last 14 days and last 6 months, read directly rather than by listing —
  // a list over every per-id marker would be thousands of keys for a handful
  // of numbers.
  for (let i = 0; i < 14; i++) {
    const d = new Date(Date.now() - i * 86400000);
    const key = utcDay(d);
    const n = (await env.PING_COUNT.get(`dau:${key}`, "number")) || 0;
    if (n > 0 || i < 7) out.people.daily[key] = n;
  }
  for (let i = 0; i < 6; i++) {
    const d = new Date();
    d.setUTCMonth(d.getUTCMonth() - i);
    const key = utcMonth(d);
    const n = (await env.PING_COUNT.get(`mau:${key}`, "number")) || 0;
    if (n > 0 || i === 0) out.people.monthly[key] = n;
  }
  out.people.active_today = out.people.daily[utcDay()] || 0;
  out.people.active_this_month = out.people.monthly[utcMonth()] || 0;

  let cursor;
  do {
    const result = await env.PING_COUNT.list({ prefix: "clipd:", cursor });
    cursor = result.cursor;
    for (const kv of result.keys) {
      const rawKey = kv.name;
      if (rawKey === "clipd:total") {
        out.starts.total = (await env.PING_COUNT.get("clipd:total", "number")) || 0;
        continue;
      }
      const parts = rawKey.split(":");
      if (parts.length !== 4 || parts[0] !== "clipd") continue;
      const [, ver, osKey, archKey] = parts;
      const count = (await env.PING_COUNT.get(rawKey, "number")) || 0;
      out.starts.by_version[ver] = (out.starts.by_version[ver] || 0) + count;
      out.starts.by_os[osKey] = (out.starts.by_os[osKey] || 0) + count;
      out.starts.by_arch[archKey] = (out.starts.by_arch[archKey] || 0) + count;
      const oa = `${osKey}:${archKey}`;
      out.starts.by_os_arch[oa] = (out.starts.by_os_arch[oa] || 0) + count;
    }
  } while (cursor);

  return new Response(JSON.stringify(out, null, 2), {
    headers: { "Content-Type": "application/json", "Cache-Control": "no-store" },
  });
}
