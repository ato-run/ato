function coreInvoke(command, payload = {}) {
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  if (typeof invoke !== "function") {
    throw new Error("No Tauri invoke bridge detected.");
  }
  return invoke(command, payload);
}

function readCapability(name) {
  const parts = name.split(".");
  let current = window;
  for (const part of parts) {
    current = current?.[part];
  }
  return current;
}

export function createTauriBridge() {
  return {
    hasBridge() {
      return typeof window.__TAURI_INTERNALS__?.invoke === "function";
    },

    async getSession() {
      return window.__DESKY_GUEST__?.session?.() ?? null;
    },

    async checkGuestMode() {
      const result = await coreInvoke("check_env", {});
      return {
        mode: result.ato_guest_mode ?? result.result?.ato_guest_mode ?? null,
        raw: result,
      };
    },

    async ping(mode, options = {}) {
      if (mode === "1") {
        return coreInvoke("ping", {
          message: options.guestMessage ?? "hello from real tauri guest",
        });
      }
      return coreInvoke("ping", {
        payload: {
          message:
            options.standaloneMessage ?? "hello from real tauri standalone",
        },
      });
    },

    async setTitle(mode, title) {
      if (mode === "1") {
        const setTitle = window.__TAURI__?.window?.getCurrent?.()?.setTitle;
        if (typeof setTitle !== "function") {
          throw new Error("window.setTitle unavailable");
        }
        return setTitle(title);
      }
      return coreInvoke("set_title", { title });
    },

    async readFile(mode, filePath) {
      if (mode === "1") {
        const readTextFile = window.__TAURI__?.fs?.readTextFile;
        if (typeof readTextFile !== "function") {
          throw new Error("fs.readTextFile unavailable");
        }
        return readTextFile(filePath);
      }
      return coreInvoke("read_file", { path: filePath });
    },

    async openExternal(url) {
      const open = window.__TAURI__?.shell?.open;
      if (typeof open !== "function") {
        throw new Error("shell.open unavailable");
      }
      return open(url);
    },

    async listenShellEvents(callback) {
      const eventApi = window.__TAURI__?.event;
      if (!eventApi?.listen) {
        return [];
      }

      const bindings = [
        "desky:shell-state",
        "desky:tab-focus",
        "desky:permission-granted",
        "desky:policy-rejected",
      ];

      const removers = await Promise.all(
        bindings.map((name) =>
          eventApi.listen(name, (payload) => callback(name, payload)),
        ),
      );
      return removers.filter(Boolean);
    },

    extractEcho(result) {
      return result?.echo ?? result?.result?.echo ?? null;
    },

    capability(name) {
      return readCapability(name);
    },
  };
}
