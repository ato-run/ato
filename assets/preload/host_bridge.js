(function () {
  if (window.__ATO_HOST__) {
    return;
  }

  const allowlist = new Set(window.__ATO_BRIDGE_ALLOWLIST__ || []);
  const endpoint = window.__ATO_BRIDGE_ENDPOINT__ || null;
  let requestId = 0;

  function nextId() {
    requestId += 1;
    return requestId;
  }

  async function sendOverProtocol(message) {
    if (!endpoint) {
      throw new Error("bridge transport unavailable for this route");
    }

    const response = await fetch(endpoint, {
      method: "POST",
      headers: {
        "content-type": "application/json",
      },
      body: JSON.stringify(message),
    });

    let body;
    try {
      body = await response.json();
    } catch (error) {
      throw new Error("bridge returned non-JSON response");
    }

    if (!response.ok || body.status !== "ok") {
      throw new Error(body.message || "bridge request failed");
    }

    return {
      requestId: body.request_id ?? null,
      message: body.message,
      payload: body.payload ?? null,
    };
  }

  function invoke(command, capability, payload) {
    const id = nextId();
    if (!allowlist.has(capability)) {
      return Promise.reject(new Error("fail-closed: capability denied: " + capability));
    }

    return sendOverProtocol({
      kind: "invoke",
      request_id: id,
      command,
      capability,
      payload: payload ?? null,
    });
  }

  window.__ATO_HOST__ = {
    allowlist: Array.from(allowlist),
    endpoint,
    can(capability) {
      return allowlist.has(capability);
    },
    invoke,
  };

  if (endpoint) {
    void sendOverProtocol({
      kind: "handshake",
      session: window.location.host || "welcome",
    }).catch(() => {});
  }

  // DevTools telemetry — send console and network events to the host panel.
  // Uses sendOverProtocol directly (bypasses capability allowlist check).
  function sendDevtools(command, payload) {
    if (!endpoint) return;
    void sendOverProtocol({
      kind: "invoke",
      request_id: nextId(),
      command,
      capability: "__devtools__",
      payload: payload,
    }).catch(() => {});
  }

  // Console interception
  const __ato_console_orig = {};
  ["log", "info", "warn", "error", "debug"].forEach(function (level) {
    __ato_console_orig[level] = console[level];
    console[level] = function () {
      __ato_console_orig[level].apply(console, arguments);
      var args = Array.prototype.slice.call(arguments);
      var message = args
        .map(function (a) {
          try {
            return typeof a === "object" ? JSON.stringify(a) : String(a);
          } catch (_) {
            return String(a);
          }
        })
        .join(" ");
      sendDevtools("devtools.console", { level: level, message: message });
    };
  });

  // Fetch interception
  var __ato_fetch_orig = window.fetch;
  window.fetch = function (input, init) {
    var reqId = Math.random().toString(36).slice(2);
    var url =
      typeof input === "string"
        ? input
        : input && input.url
          ? input.url
          : "";
    var method = (
      (init && init.method) ||
      (typeof input === "object" && input && input.method) ||
      "GET"
    ).toUpperCase();
    var t0 = Date.now();
    sendDevtools("devtools.network.start", {
      reqId: reqId,
      method: method,
      url: url,
    });
    return __ato_fetch_orig.call(this, input, init).then(
      function (response) {
        sendDevtools("devtools.network.end", {
          reqId: reqId,
          status: response.status,
          durationMs: Date.now() - t0,
        });
        return response;
      },
      function (err) {
        sendDevtools("devtools.network.end", {
          reqId: reqId,
          status: 0,
          durationMs: Date.now() - t0,
        });
        throw err;
      }
    );
  };
})();