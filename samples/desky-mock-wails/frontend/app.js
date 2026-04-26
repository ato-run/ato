const output = document.getElementById("output");
const runtimeStatus = document.getElementById("runtime-status");
const goStatus = document.getElementById("go-status");
const modeStatus = document.getElementById("mode-status");
const pingButton = document.getElementById("ping");
const goPingButton = document.getElementById("go-ping");
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

async function invokeRuntimePing() {
  const runtimeInvoke = window.runtime?.Invoke || window.runtime?.invoke;
  runtimeStatus.textContent =
    typeof runtimeInvoke === "function" ? "injected" : "missing";
  window.__DESKY_FRONTEND_STATE__.bridgeDetected =
    typeof runtimeInvoke === "function";
  if (typeof runtimeInvoke !== "function") {
    writeOutput("runtime", "No Wails-compatible runtime bridge detected.");
    return;
  }

  try {
    const result = await runtimeInvoke("ping", {
      message: "hello from wails runtime",
    });
    window.__DESKY_FRONTEND_STATE__.lastEcho =
      result.echo ?? result.result?.echo ?? null;
    writeOutput("runtime", result);
  } catch (error) {
    writeOutput("runtimeError", String(error));
  }
}

async function invokeGoPing() {
  const binding = window.go?.main?.App?.Ping;
  goStatus.textContent = typeof binding === "function" ? "injected" : "missing";
  if (typeof binding !== "function") {
    writeOutput("go", "No Wails go.* binding detected.");
    return;
  }

  try {
    const result = await binding({
      message: "hello from wails go binding",
    });
    window.__DESKY_FRONTEND_STATE__.lastEcho =
      result.echo ?? result.result?.echo ?? null;
    writeOutput("go", result);
  } catch (error) {
    writeOutput("goError", String(error));
  }
}

async function setHostTitle() {
  try {
    const result = await window.go?.main?.App?.SetTitle?.(
      "Desky Wails Mock · title updated",
    );
    writeOutput("setTitle", result ?? "ok");
  } catch (error) {
    writeOutput("setTitleError", String(error));
  }
}

async function readWorkspaceFile() {
  try {
    const contents =
      await window.go?.main?.App?.ReadFile?.("backend/server.py");
    writeOutput(
      "readFile",
      String(contents).split("\n").slice(0, 2).join("\n"),
    );
  } catch (error) {
    writeOutput("readFileError", String(error));
  }
}

async function forbiddenRead() {
  try {
    const contents = await window.go?.main?.App?.ReadFile?.("../README.md");
    writeOutput("forbiddenReadUnexpectedSuccess", contents);
  } catch (error) {
    writeOutput("forbiddenRead", String(error));
  }
}

async function checkGuestMode() {
  const runtimeInvoke = window.runtime?.Invoke || window.runtime?.invoke;
  if (typeof runtimeInvoke !== "function") {
    writeOutput("checkEnv", "No Wails-compatible runtime bridge detected.");
    return null;
  }

  try {
    const result = await runtimeInvoke("check_env", {});
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

pingButton?.addEventListener("click", invokeRuntimePing);
goPingButton?.addEventListener("click", invokeGoPing);
setTitleButton?.addEventListener("click", setHostTitle);
readFileButton?.addEventListener("click", readWorkspaceFile);
forbiddenReadButton?.addEventListener("click", forbiddenRead);

void invokeRuntimePing();
void invokeGoPing();
void (async () => {
  const guestMode = await checkGuestMode();
  if (guestMode === "1") {
    await setHostTitle();
    await readWorkspaceFile();
    await forbiddenRead();
  } else {
    writeOutput("mode", "standalone branch active");
  }
})();
