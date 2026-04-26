(function () {
  const host = window.__ATO_HOST__;
  const guestSession = window.__ATO_GUEST_SESSION__ || null;
  const listeners = new Map();

  function addListener(name, callback) {
    const current = listeners.get(name) || [];
    current.push(callback);
    listeners.set(name, current);
    return () => {
      const next = (listeners.get(name) || []).filter((candidate) => candidate !== callback);
      listeners.set(name, next);
    };
  }

  function emit(name, payload) {
    for (const callback of listeners.get(name) || []) {
      callback(payload);
    }
  }

  function invokeBackend(command, payload) {
    return host
      .invoke(command, "app.invoke", payload)
      .then((response) => response.payload ?? response);
  }

  async function invoke(channel, payload = {}) {
    switch (channel) {
      case "desky:ping":
        return invokeBackend("ping", payload);
      case "desky:invoke":
        return invokeBackend(payload.command || "ping", payload.payload || {});
      case "desky:window:setTitle":
        return host
          .invoke("plugin:window|setTitle", "plugin:window|setTitle", {
            title: payload.title || "Ato Desktop",
          })
          .then((response) => response.payload ?? response);
      case "desky:fs:readFile":
        return host
          .invoke("plugin:fs|readFile", "plugin:fs|readFile", {
            path: payload.path || "",
            encoding: payload.encoding || "utf8",
          })
          .then((response) => response.payload?.contents ?? response.payload ?? "");
      case "desky:shell:openExternal":
        return host
          .invoke("shell.open", "shell.open", {
            url: payload.url || "",
          })
          .then((response) => response.payload ?? response);
      default:
        throw new Error("Unsupported Electron guest channel: " + channel);
    }
  }

  const electronBridge = {
    ipcRenderer: {
      invoke,
      on(name, callback) {
        return addListener(name, (_payload) => callback(_payload));
      },
      removeAllListeners(name) {
        listeners.set(name, []);
      },
    },
  };

  window.__DESKY_GUEST__ = {
    session() {
      return Promise.resolve(guestSession);
    },
  };

  window.electron = electronBridge;
  window.electronAPI = {
    invoke,
  };

  queueMicrotask(() => {
    emit("desky:shell-state", {
      sessionId: guestSession?.sessionId ?? null,
      adapter: guestSession?.adapter ?? null,
      capabilities: host.allowlist,
    });
    emit("desky:permission-granted", { capabilities: host.allowlist });
  });
})();
