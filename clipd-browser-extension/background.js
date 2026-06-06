'use strict';

/**
 * clipd — Link Slot Copy  ·  background service worker
 *
 * PUSH_LINK → POST /push { url, slot }
 * PUSH_SYNC → POST /push-sync { url }; 404 → POST /push { url, fallbackSlot } if slot ≥ 2
 */

// Use 127.0.0.1 (same interface clipd binds) so we never hit ::1:51234 or another
// listener while localhost resolves to IPv6 first.
const DAEMON_PUSH = 'http://127.0.0.1:51234/push';
const DAEMON_SYNC = 'http://127.0.0.1:51234/push-sync';
const TIMEOUT_MS = 3000;
const SYNC_RETRY_MS = 14;
const SYNC_MAX_TRIES = 30;

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg.type === 'PUSH_LINK') {
    pushToDaemon(msg.url, msg.slot)
      .then(sendResponse)
      .catch((err) => sendResponse({ ok: false, error: err.message }));
    return true;
  }

  if (msg.type === 'PUSH_SYNC') {
    pushSyncToDaemon(msg.url, msg.fallbackSlot)
      .then(sendResponse)
      .catch((err) => sendResponse({ ok: false, error: err.message }));
    return true;
  }

  return false;
});

function sleep (ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function pushToDaemon (url, slot) {
  const slotN = Number(slot);
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);

  let response;
  try {
    response = await fetch(DAEMON_PUSH, {
      method:  'POST',
      headers: { 'Content-Type': 'application/json' },
      body:    JSON.stringify({ url, slot: slotN }),
      signal:  controller.signal,
    });
  } catch (err) {
    if (err.name === 'AbortError') {
      throw new Error('clipd daemon not responding. Is it running?');
    }
    throw new Error(
      'clipd daemon not running. Start it with: clipd start'
    );
  } finally {
    clearTimeout(timer);
  }

  if (!response.ok) {
    let detail = '';
    try { detail = (await response.json()).error || ''; } catch (_) {}
    if (response.status === 400 && detail.includes('slot')) {
      throw new Error(`Slot ${slot} is out of range`);
    }
    throw new Error(detail || `clipd daemon returned HTTP ${response.status}`);
  }

  const data = await response.json();
  if (!data.ok) {
    throw new Error(data.error || 'clipd: unknown error');
  }

  return { ok: true, slot: data.slot ?? slotN };
}

async function pushSyncToDaemon (url, fallbackSlot) {
  const fb = Number(fallbackSlot);
  for (let attempt = 0; attempt < SYNC_MAX_TRIES; attempt++) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);

    let response;
    try {
      response = await fetch(DAEMON_SYNC, {
        method:  'POST',
        headers: { 'Content-Type': 'application/json' },
        body:    JSON.stringify({ url }),
        signal:  controller.signal,
      });
    } catch (err) {
      if (err.name === 'AbortError') {
        throw new Error('clipd daemon not responding. Is it running?');
      }
      throw new Error(
        'clipd daemon not running. Start it with: clipd start'
      );
    } finally {
      clearTimeout(timer);
    }

    const data = await response.json().catch(() => ({}));

    if (response.ok && data.ok) {
      return { ok: true, slot: data.slot };
    }

    if (response.status === 409 && data.retry) {
      await sleep(SYNC_RETRY_MS);
      continue;
    }

    if (response.status === 503) {
      if (Number.isFinite(fb) && fb >= 2) {
        return pushToDaemon(url, fb);
      }
      throw new Error(
        data.detail || data.error ||
          'push-sync unavailable — update clipd or use Ctrl+Shift+digit on Linux'
      );
    }

    // Old clipd or wrong process on 51234: /push has existed since v1; use explicit slot.
    if (
      response.status === 404 &&
      (data.error === 'not found' || data.error === 'clipd_route_not_found')
    ) {
      if (Number.isFinite(fb) && fb >= 2) {
        return pushToDaemon(url, fb);
      }
      throw new Error(
        'clipd on port 51234 has no /push-sync and no valid tap slot to fall back to.'
      );
    }

    throw new Error(data.error || `clipd push-sync HTTP ${response.status}`);
  }

  throw new Error('clipd push-sync: tap counter never reached slot ≥2 (timed out)');
}
