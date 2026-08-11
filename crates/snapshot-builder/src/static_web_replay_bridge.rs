//! Staging-only transport adapter for the Replay PoC.
//!
//! This script deliberately owns no timeline, persistence, scheduler, or UI.
//! Those remain in ato-pwa. It only adapts authenticated parent postMessages
//! to DOM input, selected localStorage, and Math.random.

pub const REPLAY_BRIDGE_PATH: &str = "__ato/replay-bridge-v0.js";
pub const REPLAY_BRIDGE_SCRIPT_TAG: &str = r#"<script src="/__ato/replay-bridge-v0.js"></script>"#;

pub const REPLAY_BRIDGE_V0_JS: &str = r#"(function () {
  "use strict";
  var PREFIX = "ato-replay-v0:";
  var MAX_BOOTSTRAP = 65536;
  var nativeRandom = Math.random;
  var bootstrap = parseBootstrap(window.name);
  window.name = "";
  if (!bootstrap) return;
  var parentOrigin = bootstrap.parent_origin;
  var channelId = bootstrap.channel_id;
  var mode = bootstrap.mode;
  var randomValues = bootstrap.random_values || [];
  var randomCursor = 0;
  var randomUnderflow = false;
  var startedAt = performance.now();
  var removers = [];

  if ((mode === "record" || mode === "replay") && bootstrap.storage) {
    restoreStorage(bootstrap.storage);
  }
  if (mode === "record") startRecording();
  if (mode === "replay") startRandomReplay();

  window.addEventListener("message", function (event) {
    var data = event.data;
    if (event.source !== window.parent || event.origin !== parentOrigin ||
        !data || typeof data !== "object" || data.channel_id !== channelId ||
        typeof data.type !== "string") return;
    if (data.type === "ato.replay.hello.v0") {
      post({ type: "ato.replay.hello.v0.ack" });
    } else if (data.type === "ato.replay.prepare.v0") {
      post({ type: "ato.replay.prepared.v0", request_id: requestId(data),
        storage: inspectStorage(data.storage_keys) });
    } else if (data.type === "ato.replay.record.stop.v0") {
      stopRecording();
      post({ type: "ato.replay.record.stop.v0.ack", request_id: requestId(data) });
    } else if (data.type === "ato.replay.apply.v0") {
      applyBatch(data.events);
      post({ type: "ato.replay.apply.v0.ack", request_id: requestId(data) });
    } else if (data.type === "ato.replay.inspect.v0") {
      post({ type: "ato.replay.inspect.v0.result", request_id: requestId(data),
        storage: inspectStorage(data.storage_keys), random_consumed: randomCursor,
        random_underflow: randomUnderflow });
    }
  });

  readyAfterApplication();

  function parseBootstrap(name) {
    try {
      if (typeof name !== "string" || name.length > MAX_BOOTSTRAP * 2 ||
          name.indexOf(PREFIX) !== 0) return null;
      var encoded = name.slice(PREFIX.length).replace(/-/g, "+").replace(/_/g, "/");
      while (encoded.length % 4) encoded += "=";
      var binary = atob(encoded);
      if (binary.length > MAX_BOOTSTRAP) return null;
      var bytes = new Uint8Array(binary.length);
      for (var i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
      var value = JSON.parse(new TextDecoder().decode(bytes));
      if (!value || value.schema !== "ato.replay-bootstrap/v0" ||
          ["idle", "record", "replay"].indexOf(value.mode) < 0 ||
          typeof value.channel_id !== "string" || value.channel_id.length < 8 ||
          !allowedParentOrigin(value.parent_origin) ||
          !Array.isArray(value.random_values) || value.random_values.length > 100000 ||
          !value.random_values.every(function (item) {
            return typeof item === "number" && isFinite(item) && item >= 0 && item < 1;
          })) return null;
      return value;
    } catch (_) { return null; }
  }

  function allowedParentOrigin(origin) {
    if (origin === "https://app.ato.run" || origin === "https://stg-app.ato.run" ||
        origin === "https://ato.run") return true;
    try {
      var url = new URL(origin);
      return url.protocol === "http:" && (url.hostname === "localhost" || url.hostname === "127.0.0.1");
    } catch (_) { return false; }
  }

  function post(message) {
    message.channel_id = channelId;
    window.parent.postMessage(message, parentOrigin);
  }

  function requestId(data) {
    return typeof data.request_id === "string" ? data.request_id : "";
  }

  function restoreStorage(storage) {
    Object.keys(storage).slice(0, 32).forEach(function (key) {
      if (key.length > 128) return;
      var value = storage[key];
      if (value === null) localStorage.removeItem(key);
      else if (typeof value === "string") localStorage.setItem(key, value);
    });
  }

  function inspectStorage(keys) {
    var result = {};
    if (!Array.isArray(keys)) return result;
    keys.slice(0, 32).forEach(function (key) {
      if (typeof key === "string" && key.length <= 128) result[key] = localStorage.getItem(key);
    });
    return result;
  }

  function startRandomReplay() {
    Math.random = function () {
      if (randomCursor >= randomValues.length) {
        randomUnderflow = true;
        post({ type: "ato.replay.error.v0", reason: "random_exhausted" });
        return 0.5;
      }
      return randomValues[randomCursor++];
    };
  }

  function startRecording() {
    Math.random = function () {
      var value = nativeRandom();
      post({ type: "ato.replay.random.v0", value: value });
      return value;
    };
    listen("keydown", keyboardEvent, true);
    listen("keyup", keyboardEvent, true);
    listen("pointerdown", pointerEvent, true);
    listen("pointerup", pointerEvent, true);
    listen("pointercancel", pointerEvent, true);
    listen("pointermove", pointerEvent, true);
    listen("click", clickEvent, true);
    listen("scroll", scrollEvent, true);
  }

  function stopRecording() {
    Math.random = nativeRandom;
    removers.splice(0).forEach(function (remove) { remove(); });
  }

  function listen(type, mapper, capture) {
    var handler = function (event) {
      post({ type: "ato.replay.input.v0", t_us: elapsedUs(), event: mapper(event) });
    };
    document.addEventListener(type, handler, capture);
    removers.push(function () { document.removeEventListener(type, handler, capture); });
  }

  function elapsedUs() { return Math.max(0, Math.round((performance.now() - startedAt) * 1000)); }
  function normalized(value, size) { return Math.max(0, Math.min(1, size ? value / size : 0)); }
  function keyboardEvent(event) {
    return { t_us: elapsedUs(), kind: event.type, key: event.key || "", code: event.code || "",
      which: event.which || event.keyCode || 0, alt: !!event.altKey, ctrl: !!event.ctrlKey,
      meta: !!event.metaKey, shift: !!event.shiftKey };
  }
  function pointerEvent(event) {
    return { t_us: elapsedUs(), kind: event.type, pointer_id: event.pointerId || 0,
      pointer_type: event.pointerType || "mouse", x: normalized(event.clientX, innerWidth),
      y: normalized(event.clientY, innerHeight), button: event.button, buttons: event.buttons };
  }
  function clickEvent(event) {
    return { t_us: elapsedUs(), kind: "click", x: normalized(event.clientX, innerWidth),
      y: normalized(event.clientY, innerHeight), button: event.button };
  }
  function scrollEvent() {
    return { t_us: elapsedUs(), kind: "scroll", x: scrollX, y: scrollY };
  }

  function applyBatch(events) {
    if (!Array.isArray(events) || events.length > 1000) return;
    events.slice().sort(function (a, b) { return a.seq - b.seq; }).forEach(applyInput);
  }

  function applyInput(input) {
    if (!input || typeof input.kind !== "string") return;
    if (input.kind === "keydown" || input.kind === "keyup") {
      var keyboard = new KeyboardEvent(input.kind, { key: input.key, code: input.code, bubbles: true,
        cancelable: true, altKey: !!input.alt, ctrlKey: !!input.ctrl,
        metaKey: !!input.meta, shiftKey: !!input.shift });
      defineLegacyKey(keyboard, "which", input.which);
      defineLegacyKey(keyboard, "keyCode", input.which);
      document.dispatchEvent(keyboard);
    } else if (input.kind === "click") {
      var cx = clampPoint(input.x, innerWidth), cy = clampPoint(input.y, innerHeight);
      var target = document.elementFromPoint(cx, cy) || document;
      target.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true,
        clientX: cx, clientY: cy, button: input.button || 0 }));
    } else if (input.kind === "scroll") {
      scrollTo(Number(input.x) || 0, Number(input.y) || 0);
    } else if (["pointerdown", "pointerup", "pointercancel", "pointermove"].indexOf(input.kind) >= 0) {
      var px = clampPoint(input.x, innerWidth), py = clampPoint(input.y, innerHeight);
      var pointerTarget = document.elementFromPoint(px, py) || document;
      var EventType = window.PointerEvent || window.MouseEvent;
      pointerTarget.dispatchEvent(new EventType(input.kind, { bubbles: true, cancelable: true,
        clientX: px, clientY: py, button: input.button, buttons: input.buttons,
        pointerId: input.pointer_id, pointerType: input.pointer_type }));
    }
  }

  function clampPoint(value, size) {
    return Math.max(0, Math.min(size - 1, Number(value) * size));
  }
  function defineLegacyKey(event, property, value) {
    try { Object.defineProperty(event, property, { get: function () { return Number(value) || 0; } }); }
    catch (_) {}
  }
  function readyAfterApplication() {
    var signal = function () {
      requestAnimationFrame(function () { requestAnimationFrame(function () {
        post({ type: "ato.replay.ready.v0" });
      }); });
    };
    if (document.readyState === "complete") signal();
    else window.addEventListener("load", signal, { once: true });
  }
}());
"#;
