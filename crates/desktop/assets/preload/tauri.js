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

  function invoke(command, payload) {
    return host
      .invoke(command, "app.invoke", payload)
      .then((response) => response.payload ?? response);
  }

  window.__DESKY_GUEST__ = {
    session() {
      return Promise.resolve(guestSession);
    },
  };

  window.__TAURI_INTERNALS__ = {
    invoke,
  };

  window.__TAURI__ = {
    core: {
      invoke,
    },
    fs: {
      readTextFile(path) {
        return host
          .invoke("plugin:fs|readFile", "plugin:fs|readFile", {
            path,
            encoding: "utf-8",
          })
          .then((response) => response.payload?.contents ?? response.payload ?? "");
      },
    },
    dialog: {
      open(options) {
        return host
          .invoke("plugin:dialog|open", "plugin:dialog|open", options || {})
          .then((response) => response.payload ?? response);
      },
    },
    event: {
      listen(name, callback) {
        return Promise.resolve(addListener(name, callback));
      },
    },
    shell: {
      open(url) {
        return host
          .invoke("shell.open", "shell.open", { url })
          .then((response) => response.payload ?? response);
      },
    },
    window: {
      getCurrent() {
        return {
          setTitle(title) {
            return host
              .invoke("plugin:window|setTitle", "plugin:window|setTitle", {
                title,
              })
              .then((response) => response.payload ?? response);
          },
        };
      },
    },
  };

  queueMicrotask(() => {
    emit("desky:shell-state", {
      sessionId: guestSession?.sessionId ?? null,
      adapter: guestSession?.adapter ?? null,
      capabilities: host.allowlist,
    });
    emit("desky:tab-focus", { focused: true });
    emit("desky:permission-granted", { capabilities: host.allowlist });
  });
})();
