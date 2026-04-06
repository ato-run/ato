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
})();