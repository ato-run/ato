/**
 * Ato Browser Instance State Bridge v1.
 *
 * Injected as a PARSER-BLOCKING classic script ahead of every application
 * script in a Static Web entry document. It has exactly one job: make the
 * owning ComputeInstance's `localStorage` state present BEFORE the App's own
 * scripts read it, and persist subsequent mutations back.
 *
 * This is the State lane of the Browser boundary. It is deliberately NOT the
 * Operation lane: a `localStorage.setItem()` is not an `ato.browser@1`
 * operation Record. A Record is evidence of how the computation was operated;
 * InstanceState is which data currently remains. Writing storage mutations
 * into the operation stream would conflate the two.
 *
 * The bridge is INERT unless an instance state document was injected into the
 * served entry HTML. The artifact is content-addressed and shared — the same
 * bytes are served on the public Static Web lane, where no ComputeInstance
 * exists — so absent (or malformed) injected state the bridge hydrates
 * nothing, patches nothing and never talks to the network.
 *
 * State shape is `ato.materialize.browser@1`'s BrowserStateV1:
 *   { "version": 1, "local_storage": [ { "key": "...", "value": "..." } ] }
 */
(function () {
  "use strict";

  var STATE_ELEMENT_ID = "__ato_instance_state_v1";
  var ENDPOINT = "/__ato/instance-state/local-storage";
  var PROTOCOL = "ato.browser-instance-state@1";
  var STATE_VERSION = 1;

  /** Mutations are batched this long so a burst of setItem calls is one POST. */
  var BATCH_MS = 150;
  /** Catches writes the Storage API patch cannot see (property assignment). */
  var SNAPSHOT_MS = 1000;
  /** Only after this many consecutive failed flushes do we tell the user. */
  var FAILURES_BEFORE_NOTICE = 3;

  var storage;
  try {
    storage = window.localStorage;
  } catch (error) {
    // Storage disabled by the browser (blocked cookies, some private modes).
    // Nothing can be hydrated or persisted; leave the App entirely alone.
    return;
  }
  if (!storage) return;

  var injected = readInjectedState();
  if (!injected) return;

  // Capture the pristine methods BEFORE any App script can wrap them, and use
  // only these captured references from here on. An App that replaces
  // `localStorage.setItem` later still reaches real storage through our patch,
  // and our own hydration never re-enters App code.
  var rawGetItem = storage.getItem.bind(storage);
  var rawSetItem = storage.setItem.bind(storage);
  var rawRemoveItem = storage.removeItem.bind(storage);
  var rawClear = storage.clear.bind(storage);
  var rawKey = storage.key.bind(storage);

  hydrate(injected);

  var snapshot = readSnapshot();
  /** Pending mutations, coalesced per key; a `clear` collapses everything. */
  var pending = [];
  var pendingClear = false;
  var batchTimer = null;
  var flushing = false;
  var consecutiveFailures = 0;
  var noticeShown = false;

  patchStorageApi();
  window.setInterval(reconcileSnapshot, SNAPSHOT_MS);

  document.addEventListener("visibilitychange", function () {
    if (document.visibilityState === "hidden") flushNow(true);
  });
  window.addEventListener("pagehide", function () {
    flushNow(true);
  });

  // The only surface the bridge exposes to the page. `flush()` lets an App
  // force a save at a known-good point (and lets acceptance tests be
  // deterministic instead of sleeping on the batch timer).
  try {
    Object.defineProperty(window, "__atoInstanceState", {
      value: Object.freeze({
        version: STATE_VERSION,
        flush: function () {
          reconcileSnapshot();
          return flushNow(false);
        },
      }),
      configurable: false,
      enumerable: false,
      writable: false,
    });
  } catch (error) {
    /* A page that already defined the name keeps it; the bridge still works. */
  }

  // ---------------------------------------------------------------- hydration

  function readInjectedState() {
    var element = document.getElementById(STATE_ELEMENT_ID);
    if (!element) return null;
    var raw = element.textContent;
    if (!raw) return null;
    var parsed;
    try {
      parsed = JSON.parse(raw);
    } catch (error) {
      return null;
    }
    // `null` is the artifact's placeholder: a document served WITHOUT an
    // instance behind it. Treat it exactly like a missing element.
    if (!parsed || typeof parsed !== "object") return null;
    if (parsed.version !== STATE_VERSION) return null;
    if (!Array.isArray(parsed.local_storage)) return null;
    var entries = [];
    for (var index = 0; index < parsed.local_storage.length; index += 1) {
      var entry = parsed.local_storage[index];
      if (!entry || typeof entry !== "object") return null;
      if (typeof entry.key !== "string" || typeof entry.value !== "string") {
        return null;
      }
      entries.push({ key: entry.key, value: entry.value });
    }
    return entries;
  }

  /**
   * The instance's state REPLACES whatever this browser profile happens to
   * hold. The server-side state is authoritative: leaving stale local keys
   * behind would let one device's leftovers masquerade as instance data.
   */
  function hydrate(entries) {
    try {
      storage.clear();
      for (var index = 0; index < entries.length; index += 1) {
        storage.setItem(entries[index].key, entries[index].value);
      }
    } catch (error) {
      /* Quota or a hostile Storage shim: the App simply starts from what
         landed. The snapshot reconciler will report the real state back. */
    }
  }

  // ------------------------------------------------------------ change capture

  function readSnapshot() {
    var current = Object.create(null);
    try {
      for (var index = 0; index < storage.length; index += 1) {
        var key = rawKey(index);
        if (typeof key !== "string") continue;
        var value = rawGetItem(key);
        if (typeof value === "string") current[key] = value;
      }
    } catch (error) {
      return Object.create(null);
    }
    return current;
  }

  function patchStorageApi() {
    try {
      storage.setItem = function (key, value) {
        var normalizedKey = String(key);
        var normalizedValue = String(value);
        rawSetItem(normalizedKey, normalizedValue);
        recordSet(normalizedKey, normalizedValue);
      };
      storage.removeItem = function (key) {
        var normalizedKey = String(key);
        rawRemoveItem(normalizedKey);
        recordRemove(normalizedKey);
      };
      storage.clear = function () {
        rawClear();
        recordClear();
      };
    } catch (error) {
      /* Storage methods are not writable here; SNAPSHOT_MS still covers us. */
    }
  }

  /**
   * Picks up writes the patched methods never saw — `localStorage.key = value`,
   * `delete localStorage.key`, and any write made through a different
   * reference to the Storage object. Diffing against the last known snapshot
   * keeps this O(keys) and emits nothing when nothing changed.
   */
  function reconcileSnapshot() {
    var current = readSnapshot();
    var key;
    for (key in current) {
      if (snapshot[key] !== current[key]) recordSet(key, current[key]);
    }
    for (key in snapshot) {
      if (!(key in current)) recordRemove(key);
    }
  }

  function recordSet(key, value) {
    snapshot[key] = value;
    enqueue({ kind: "set", key: key, value: value });
  }

  function recordRemove(key) {
    delete snapshot[key];
    enqueue({ kind: "remove", key: key });
  }

  function recordClear() {
    snapshot = Object.create(null);
    // A clear supersedes every not-yet-sent per-key operation.
    pending = [];
    pendingClear = true;
    scheduleFlush();
  }

  /**
   * Coalesces per key: only the LAST pending operation for a key is worth
   * sending, so a tight update loop still produces one operation per key.
   */
  function enqueue(operation) {
    for (var index = 0; index < pending.length; index += 1) {
      if (pending[index].key === operation.key) {
        pending[index] = operation;
        scheduleFlush();
        return;
      }
    }
    pending.push(operation);
    scheduleFlush();
  }

  function scheduleFlush() {
    if (batchTimer !== null) return;
    batchTimer = window.setTimeout(function () {
      batchTimer = null;
      flushNow(false);
    }, BATCH_MS);
  }

  // ------------------------------------------------------------------- flush

  function flushNow(unloading) {
    if (batchTimer !== null) {
      window.clearTimeout(batchTimer);
      batchTimer = null;
    }
    if (flushing && !unloading) return Promise.resolve(false);
    var operations = buildOperations();
    if (operations.length === 0) return Promise.resolve(true);

    // Claim the batch before awaiting so concurrent mutations accumulate into
    // the NEXT batch rather than being dropped or sent twice.
    var claimedClear = pendingClear;
    var claimedPending = pending;
    pendingClear = false;
    pending = [];
    flushing = true;

    var body = JSON.stringify({ protocol: PROTOCOL, operations: operations });
    return fetch(ENDPOINT, {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      keepalive: true,
      headers: { "content-type": "application/json" },
      body: body,
    })
      .then(function (response) {
        if (!response.ok) throw new Error("save rejected: " + response.status);
        flushing = false;
        consecutiveFailures = 0;
        hideNotice();
        return true;
      })
      .catch(function () {
        flushing = false;
        // Put the batch back so the next flush retries it. Anything enqueued
        // meanwhile is newer and must win, so the replayed batch goes first.
        pendingClear = pendingClear || claimedClear;
        pending = claimedPending.concat(pending);
        consecutiveFailures += 1;
        if (consecutiveFailures >= FAILURES_BEFORE_NOTICE) showNotice();
        return false;
      });
  }

  function buildOperations() {
    var operations = [];
    if (pendingClear) operations.push({ kind: "clear" });
    for (var index = 0; index < pending.length; index += 1) {
      operations.push(pending[index]);
    }
    return operations;
  }

  // ------------------------------------------------------------------ notice

  /**
   * Deliberately silent in the normal case. Saving is the whole promise of a
   * personal App, so a SUSTAINED failure to save is the one thing the bridge
   * must not hide — but a single dropped request during a network blip is not
   * worth interrupting anyone over.
   */
  function showNotice() {
    if (noticeShown) return;
    noticeShown = true;
    try {
      var notice = document.createElement("div");
      notice.id = "__ato-instance-state-notice";
      notice.textContent = "Changes are not being saved";
      notice.setAttribute("role", "status");
      notice.style.cssText = [
        "position:fixed",
        "z-index:2147483647",
        "left:50%",
        "bottom:16px",
        "transform:translateX(-50%)",
        "max-width:calc(100vw - 32px)",
        "padding:8px 14px",
        "border-radius:8px",
        "background:#1f2937",
        "color:#fff",
        "font:500 13px/1.4 system-ui,-apple-system,sans-serif",
        "box-shadow:0 4px 16px rgba(0,0,0,.24)",
        "pointer-events:none",
      ].join(";");
      (document.body || document.documentElement).appendChild(notice);
    } catch (error) {
      /* Nothing to attach to yet; the next failure will try again. */
    }
  }

  function hideNotice() {
    if (!noticeShown) return;
    noticeShown = false;
    try {
      var notice = document.getElementById("__ato-instance-state-notice");
      if (notice && notice.parentNode) notice.parentNode.removeChild(notice);
    } catch (error) {
      /* Already gone. */
    }
  }
})();
