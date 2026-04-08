export function createElectronBridge() {
  return {
    hasBridge() {
      return typeof window.electron?.ipcRenderer?.invoke === "function";
    },

    invoke(channel, payload = {}) {
      const invoke = window.electron?.ipcRenderer?.invoke;
      if (typeof invoke !== "function") {
        throw new Error("No Electron preload bridge detected.");
      }
      return invoke(channel, payload);
    },

    async ping() {
      return this.invoke("desky:ping", {
        message: "hello from real electron guest",
      });
    },

    async setTitle(title) {
      return this.invoke("desky:window:setTitle", { title });
    },

    async readFile(path, encoding = "utf8") {
      return this.invoke("desky:fs:readFile", { path, encoding });
    },

    async checkGuestMode() {
      const result = await this.invoke("desky:invoke", {
        command: "check_env",
        payload: {},
      });
      return {
        mode: result.ato_guest_mode ?? result.result?.ato_guest_mode ?? null,
        raw: result,
      };
    },

    extractEcho(result) {
      return result?.echo ?? result?.result?.echo ?? null;
    },
  };
}
