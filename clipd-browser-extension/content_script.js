/**
 * clipd — Link Slot Copy  ·  content script
 *
 * Cmd+C ×N while hovering a link:
 *   Clipboard relay — the copy event handler writes the hovered URL to the
 *   OS clipboard AFTER the browser finishes its own copy, so the daemon reads
 *   the URL (not empty / selected text) when its 500 ms window expires.
 *
 * Cmd+V ×N (paste):
 *   e.preventDefault() on every intermediate tap so Chrome doesn't paste from
 *   the clipboard on each keystroke.  The daemon fires after 500 ms, simulates
 *   one final Cmd+V, and Chrome pastes the correct slot content then.
 *
 * ⌘⇧+1–9 / Ctrl⇧+1–9 → POST /push with an explicit slot number.
 */

;(function () {
  'use strict';

  if (window !== window.top) return;

  const isMac = /Mac|iPhone|iPod|iPad/.test(navigator.platform);
  console.log('[clipd] loaded — isMac:', isMac, location.href);

  const TAP_WINDOW_MS = 500;
  const MAX_CLIP_SLOT = 30;

  let currentHoveredElement = null;

  // Cmd+C state
  let chordTaps  = 0;
  let chordTimer = null;
  let chordHref  = null;

  // Cmd+V state — track taps so we only block taps 2+ (tap 1 must reach the app)
  let pasteTaps  = 0;
  let pasteTimer = null;

  // ── Hover tracking ─────────────────────────────────────────────────────────
  document.addEventListener('mouseover', (e) => {
    currentHoveredElement = e.target;
  }, { passive: true, capture: true });

  document.addEventListener('mouseout', (e) => {
    if (currentHoveredElement === e.target) currentHoveredElement = null;
  }, { passive: true, capture: true });

  // ── DOM walk: nearest <a href> ─────────────────────────────────────────────
  function findHoveredHref (el) {
    if (!el) return null;
    let node = el;
    while (node && node !== document.documentElement) {
      if (node.tagName === 'A') {
        const href = node.href || node.getAttribute('href');
        if (href && href !== '#' && !href.startsWith('javascript:')) return href;
      }
      node = node.parentElement;
    }
    return null;
  }

  // ── Chord detection ────────────────────────────────────────────────────────
  function isCopyChord (e) {
    return isMac
      ? (e.metaKey && !e.shiftKey && !e.altKey && !e.ctrlKey && e.key === 'c')
      : (e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && e.key === 'c');
  }

  function isPasteChord (e) {
    return isMac
      ? (e.metaKey && !e.shiftKey && !e.altKey && !e.ctrlKey && e.key === 'v')
      : (e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && e.key === 'v');
  }

  function slotFromChord (e) {
    const modOk = isMac
      ? (e.metaKey && e.shiftKey && !e.ctrlKey && !e.altKey)
      : (e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey);
    if (!modOk) return null;
    const m = e.code.match(/^(?:Digit|Numpad)([1-9])$/);
    return m ? parseInt(m[1], 10) : null;
  }

  function isModifierOnly (e) {
    const c = e.code || '';
    return (
      c === 'MetaLeft'    || c === 'MetaRight'    ||
      c === 'ControlLeft' || c === 'ControlRight' ||
      c === 'ShiftLeft'   || c === 'ShiftRight'   ||
      c === 'AltLeft'     || c === 'AltRight'     ||
      c === 'OSLeft'      || c === 'OSRight'
    );
  }

  function resetChord (reason) {
    console.log('[clipd] reset —', reason, '| was:', chordTaps);
    chordTaps = 0;
    chordHref = null;
    clearTimeout(chordTimer);
    chordTimer = null;
  }

  // ── sendMessage with service-worker wake-up retry ─────────────────────────
  function sendMessageRetry (payload, cb, attempt = 0) {
    try {
      chrome.runtime.sendMessage(payload, (res) => {
        if (chrome.runtime.lastError) {
          if (attempt < 12) {
            setTimeout(() => sendMessageRetry(payload, cb, attempt + 1), 70);
            return;
          }
          console.warn('[clipd] SW:', chrome.runtime.lastError.message);
          return;
        }
        cb(res);
      });
    } catch (err) {
      console.warn('[clipd] sendMessage:', err);
    }
  }

  // ── copy event: clipboard relay ────────────────────────────────────────────
  // The browser fires the copy event AFTER keydown and writes selected text
  // (often empty) to the clipboard.  We use setTimeout(0) to run AFTER that
  // write completes so the URL overwrites it — no e.preventDefault() needed,
  // which means Chrome keeps delivering all future Cmd+C keydowns freely.
  document.addEventListener('copy', (e) => {
    if (chordTaps >= 2 && chordHref) {
      const url = chordHref;
      setTimeout(() => {
        navigator.clipboard.writeText(url).catch((err) => {
          console.warn('[clipd] clipboard relay failed:', err.message);
        });
      }, 0);
    }
  }, { capture: true });

  // ── keydown ────────────────────────────────────────────────────────────────
  document.addEventListener('keydown', (e) => {

    // ── Cmd+V: allow tap 1 (natural paste), block taps 2+ ──────────────────
    // Tap 1 must reach the app so a normal single Cmd+V works as expected.
    // Taps 2, 3 … N are suppressed — the daemon waits 500 ms, counts all taps,
    // undoes the one natural paste (tap 1), then pastes from the correct slot.
    // The daemon's own simulated final Cmd+V arrives after the 500 ms window
    // has closed and pasteTaps has been reset to 0, so it is never blocked.
    if (isPasteChord(e)) {
      if (e.repeat) return;
      pasteTaps++;
      clearTimeout(pasteTimer);
      pasteTimer = setTimeout(() => { pasteTaps = 0; }, TAP_WINDOW_MS);
      if (pasteTaps >= 2) {
        e.preventDefault();  // block intermediate taps so Chrome doesn't paste N times
      }
      return;
    }

    // ── Cmd+C: count taps and relay URL via copy event ───────────────────────
    if (isCopyChord(e)) {
      if (e.repeat) return;

      const hoveredHref = findHoveredHref(currentHoveredElement);
      const href = hoveredHref || (chordTaps > 0 ? chordHref : null);

      if (!href) { resetChord('no-href'); return; }

      if (chordTaps === 0) chordHref = href;
      chordTaps = Math.min(chordTaps + 1, MAX_CLIP_SLOT);

      console.log('[clipd] Cmd+C tap', chordTaps, '→', href);

      clearTimeout(chordTimer);
      chordTimer = setTimeout(() => {
        if (chordTaps >= 2 && chordHref) {
          const url = chordHref;
          const fallbackSlot = chordTaps;
          sendMessageRetry({ type: 'PUSH_SYNC', url, fallbackSlot }, (res) => {
            if (res && res.ok) {
              console.log('[clipd] PUSH_SYNC → slot', res.slot);
            } else {
              console.warn('[clipd] PUSH_SYNC failed:', res && res.error);
            }
          });
        }
        resetChord('idle');
      }, TAP_WINDOW_MS);

      // Clipboard relay happens in the copy event handler (fired after this
      // keydown), not here — so it runs after the browser's own copy write.
      return;
    }

    // ── ⌘⇧+1–9 / Ctrl⇧+1–9: explicit slot ──────────────────────────────────
    const slot = slotFromChord(e);
    if (slot !== null) {
      const href = findHoveredHref(currentHoveredElement);
      if (!href) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      sendMessageRetry({ type: 'PUSH_LINK', url: href, slot }, (res) => {
        if (res && res.ok) {
          navigator.clipboard.writeText(href).catch(() => {});
          console.log('[clipd] explicit → slot', res.slot);
        } else {
          console.warn('[clipd] PUSH_LINK failed:', res && res.error);
        }
      });
      return;
    }

    // Any non-modifier key resets the Cmd+C chord.
    if (chordTaps > 0) {
      if (isModifierOnly(e)) return;
      if (e.key === 'Escape') { resetChord('escape'); return; }
      if (isMac ? e.metaKey : e.ctrlKey) return;
      resetChord('other-key');
    }
  }, { capture: true });

})();
