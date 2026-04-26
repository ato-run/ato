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

  async function runtimeInvoke(command, payload = {}) {
    return invokeBackend(command, payload);
  }

  function goBinding(name) {
    switch (name) {
      case "Ping":
        return (payload = {}) => invokeBackend("ping", payload);
      case "SetTitle":
        return (title) =>
          host
            .invoke("plugin:window|setTitle", "plugin:window|setTitle", {
              title,
            })
            .then((response) => response.payload ?? response);
      case "ReadFile":
        return (path) =>
          host
            .invoke("plugin:fs|readFile", "plugin:fs|readFile", {
              path,
              encoding: "utf-8",
            })
            .then((response) => response.payload?.contents ?? response.payload ?? "");
      case "CheckEnv":
        return () => invokeBackend("check_env", {});
      default:
        return undefined;
    }
  }

  window.__DESKY_GUEST__ = {
    session() {
      return Promise.resolve(guestSession);
    },
  };

  window.runtime = {
    Invoke: runtimeInvoke,
    invoke: runtimeInvoke,
    EventsEmit(name, payload) {
      emit(name, payload);
      return Promise.resolve({ ok: true });
    },
    EventsOn(name, callback) {
      return Promise.resolve(addListener(name, callback));
    },
  };

  window.go = {
    main: {
      App: {
        Ping: goBinding("Ping"),
        SetTitle: goBinding("SetTitle"),
        ReadFile: goBinding("ReadFile"),
        CheckEnv: goBinding("CheckEnv"),
      },
    },
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
