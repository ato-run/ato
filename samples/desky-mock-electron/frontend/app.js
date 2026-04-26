const output = document.getElementById("output");
const modeStatus = document.getElementById("mode-status");
const pingButton = document.getElementById("ping");
const setTitleButton = document.getElementById("set-title");
const readFileButton = document.getElementById("read-file");
const forbiddenReadButton = document.getElementById("forbidden-read");

window.__DESKY_FRONTEND_STATE__ = {
  rendered: true,
  bridgeDetected: false,
  sessionReady: true,
  eventCount: 0,
  lastEcho: null,
  lastGuestMode: null,
  lastShellEvent: null,
};

function writeOutput(label, value) {
  output.textContent = `${label}: ${typeof value === "string" ? value : JSON.stringify(value, null, 2)}`;
}

function applyModeLabel(mode) {
  if (!modeStatus) {
    return;
  }
  modeStatus.textContent =
    mode === "1" ? "Desky guest mode" : "Standalone mode";
}

function ipcInvoke(channel, payload = {}) {
  const invoke = window.electron?.ipcRenderer?.invoke;
  if (typeof invoke !== "function") {
    throw new Error("No Electron guest bridge detected.");
  }
  window.__DESKY_FRONTEND_STATE__.bridgeDetected = true;
  return invoke(channel, payload);
}

async function invokePing() {
  try {
    const result = await ipcInvoke("desky:ping", {
      message: "hello from electron guest",
    });
    window.__DESKY_FRONTEND_STATE__.lastEcho =
      result.echo ?? result.result?.echo ?? null;
    writeOutput("ping", result);
  } catch (error) {
    writeOutput("pingError", String(error));
  }
}

async function setTitle() {
  try {
    const result = await ipcInvoke("desky:window:setTitle", {
      title: "Desky Electron Mock · title updated",
    });
    writeOutput("setTitle", result);
  } catch (error) {
    writeOutput("setTitleError", String(error));
  }
}

async function readFile() {
  try {
    const result = await ipcInvoke("desky:fs:readFile", {
      path: "backend/server.py",
      encoding: "utf8",
    });
    writeOutput("readFile", String(result).split("\n").slice(0, 2).join("\n"));
  } catch (error) {
    writeOutput("readFileError", String(error));
  }
}

async function forbiddenRead() {
  try {
    const result = await ipcInvoke("desky:fs:readFile", {
      path: "../README.md",
      encoding: "utf8",
    });
    writeOutput("forbiddenReadUnexpectedSuccess", result);
  } catch (error) {
    writeOutput("forbiddenRead", String(error));
  }
}

async function checkGuestMode() {
  try {
    const result = await ipcInvoke("desky:invoke", {
      command: "check_env",
      payload: {},
    });
    const mode = result.ato_guest_mode ?? result.result?.ato_guest_mode ?? null;
    window.__DESKY_FRONTEND_STATE__.lastGuestMode = mode;
    applyModeLabel(mode);
    writeOutput("checkEnv", result);
    return mode;
  } catch (error) {
    writeOutput("checkEnvError", String(error));
    return null;
  }
}

pingButton?.addEventListener("click", invokePing);
setTitleButton?.addEventListener("click", setTitle);
readFileButton?.addEventListener("click", readFile);
forbiddenReadButton?.addEventListener("click", forbiddenRead);

void invokePing();
void (async () => {
  const guestMode = await checkGuestMode();
  if (guestMode === "1") {
    await setTitle();
    await readFile();
    await forbiddenRead();
  } else {
    writeOutput("mode", "standalone branch active");
  }
})();
