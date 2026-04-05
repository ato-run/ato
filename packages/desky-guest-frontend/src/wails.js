function runtimeInvoke() {
  return window.runtime?.Invoke || window.runtime?.invoke || null;
}

function goBinding(name) {
  return window.go?.main?.App?.[name] || null;
}

export function createWailsBridge() {
  return {
    hasRuntime() {
      return typeof runtimeInvoke() === "function";
    },

    hasGoBinding(name) {
      return typeof goBinding(name) === "function";
    },

    async ping() {
      const runtimeCall = runtimeInvoke();
      if (typeof runtimeCall === "function") {
        const result = await runtimeCall("ping", {
          message: "hello from real wails runtime",
        });
        return { source: "runtime", raw: result };
      }

      const binding = goBinding("Ping");
      if (typeof binding !== "function") {
        throw new Error("No Wails runtime or go binding detected.");
      }

      const result = await binding({
        message: "hello from real wails go binding",
      });
      return { source: "go", raw: result };
    },

    async pingGo() {
      const binding = goBinding("Ping");
      if (typeof binding !== "function") {
        throw new Error("No Wails go.* binding detected.");
      }
      return binding({
        message: "hello from real wails go binding",
      });
    },

    async checkGuestMode() {
      const runtimeCall = runtimeInvoke();
      if (typeof runtimeCall === "function") {
        const result = await runtimeCall("check_env", {});
        return {
          mode: result.ato_guest_mode ?? result.result?.ato_guest_mode ?? null,
          raw: result,
        };
      }

      const binding = goBinding("CheckEnv");
      if (typeof binding !== "function") {
        throw new Error("No Wails CheckEnv binding detected.");
      }
      const result = await binding();
      return {
        mode: result.ato_guest_mode ?? null,
        raw: result,
      };
    },

    async setTitle(title) {
      const binding = goBinding("SetTitle");
      if (typeof binding !== "function") {
        throw new Error("No Wails SetTitle binding detected.");
      }
      return binding(title);
    },

    async readFile(path) {
      const binding = goBinding("ReadFile");
      if (typeof binding !== "function") {
        throw new Error("No Wails ReadFile binding detected.");
      }
      return binding(path);
    },

    extractEcho(result) {
      return result?.echo ?? result?.result?.echo ?? null;
    },
  };
}
