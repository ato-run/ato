const output = document.getElementById("output");
const bridgeStatus = document.getElementById("bridge-status");
const sessionStatus = document.getElementById("session-status");
const modeStatus = document.getElementById("mode-status");
const lastEcho = document.getElementById("last-echo");
const eventCount = document.getElementById("event-count");
const eventLog = document.getElementById("event-log");
const badgeLabel = document.getElementById("badge-label");
const pingButton = document.getElementById("ping");
const setTitleButton = document.getElementById("set-title");
const readFileButton = document.getElementById("read-file");
const forbiddenReadButton = document.getElementById("forbidden-read");
const externalButton = document.getElementById("external");
const capabilityLog = document.getElementById("capability-log");

window.__DESKY_FRONTEND_STATE__ = {
  rendered: true,
  bridgeDetected: false,
  sessionReady: false,
  eventCount: 0,
  lastEcho: null,
  lastGuestMode: null,
  lastShellEvent: null,
  lastWindowTitle: null,
  lastFileRead: null,
  lastPolicyError: null,
};

function setFrontendState(patch) {
  Object.assign(window.__DESKY_FRONTEND_STATE__, patch);
}

function renderStateLine(label, value) {
  return `${label}: ${typeof value === "string" ? value : JSON.stringify(value, null, 2)}`;
}

function updateStatusView(extra = []) {
  output.textContent = [
    renderStateLine("bridge", window.__DESKY_FRONTEND_STATE__.bridgeDetected),
    renderStateLine(
      "sessionReady",
      window.__DESKY_FRONTEND_STATE__.sessionReady,
    ),
    renderStateLine("lastEcho", window.__DESKY_FRONTEND_STATE__.lastEcho),
    renderStateLine(
      "lastGuestMode",
      window.__DESKY_FRONTEND_STATE__.lastGuestMode,
    ),
    renderStateLine(
      "lastWindowTitle",
      window.__DESKY_FRONTEND_STATE__.lastWindowTitle,
    ),
    renderStateLine(
      "lastFileRead",
      window.__DESKY_FRONTEND_STATE__.lastFileRead,
    ),
    renderStateLine(
      "lastPolicyError",
      window.__DESKY_FRONTEND_STATE__.lastPolicyError,
    ),
    renderStateLine(
      "lastShellEvent",
      window.__DESKY_FRONTEND_STATE__.lastShellEvent,
    ),
    ...extra,
  ].join("\n");
}

function applyShellEvent(name, payload) {
  setFrontendState({
    eventCount: window.__DESKY_FRONTEND_STATE__.eventCount + 1,
    lastShellEvent: name,
  });
  eventCount.textContent = String(window.__DESKY_FRONTEND_STATE__.eventCount);
  eventLog.textContent = JSON.stringify({ name, payload }, null, 2);
  updateStatusView();
}

function updateCapabilityLog(label, value) {
  capabilityLog.textContent = `${label}: ${typeof value === "string" ? value : JSON.stringify(value, null, 2)}`;
}

function applyModeLabel(mode) {
  const label = mode === "1" ? "Desky guest mode" : "Standalone mode";
  if (modeStatus) {
    modeStatus.textContent = label;
  }
  badgeLabel.textContent = label;
}

async function invokePing(message = "hello from tauri frontend") {
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  if (typeof invoke !== "function") {
    bridgeStatus.textContent = "missing";
    badgeLabel.textContent = "No Tauri bridge detected";
    updateStatusView(["error: missing Tauri-compatible bridge"]);
    return;
  }

  try {
    const result = await invoke("ping", { message });
    setFrontendState({ lastEcho: result.echo ?? result.result?.echo ?? null });
    lastEcho.textContent = window.__DESKY_FRONTEND_STATE__.lastEcho ?? "-";
    document.title = `Desky Mock Tauri · ${window.__DESKY_FRONTEND_STATE__.lastEcho ?? "ready"}`;
    updateStatusView([JSON.stringify(result, null, 2)]);
  } catch (error) {
    updateStatusView([`invokeError: ${String(error)}`]);
  }
}

async function requestExternalOpen() {
  const open = window.__TAURI__?.shell?.open;
  if (typeof open !== "function") {
    updateStatusView(["shell.open unavailable"]);
    return;
  }
  try {
    await open("https://example.com/");
    updateStatusView(["shell.open request issued"]);
  } catch (error) {
    updateStatusView([`shell.open error: ${String(error)}`]);
  }
}

async function setHostTitle(title = "Desky Tauri Mock · host title set") {
  const setTitle = window.__TAURI__?.window?.getCurrent?.()?.setTitle;
  if (typeof setTitle !== "function") {
    updateCapabilityLog("setTitle", "window.setTitle unavailable");
    return;
  }
  try {
    const result = await setTitle(title);
    setFrontendState({ lastWindowTitle: result?.title ?? title });
    updateCapabilityLog("setTitle", result ?? title);
    updateStatusView();
  } catch (error) {
    updateCapabilityLog("setTitleError", String(error));
  }
}

async function readWorkspaceFile(filePath = "backend/server.py") {
  const readTextFile = window.__TAURI__?.fs?.readTextFile;
  if (typeof readTextFile !== "function") {
    updateCapabilityLog("readTextFile", "fs.readTextFile unavailable");
    return;
  }
  try {
    const contents = await readTextFile(filePath);
    const summary = contents.split("\n").slice(0, 2).join("\n");
    setFrontendState({ lastFileRead: summary });
    updateCapabilityLog("readTextFile", summary);
    updateStatusView();
  } catch (error) {
    updateCapabilityLog("readTextFileError", String(error));
  }
}

async function readOutsideBoundary(filePath = "../README.md") {
  const readTextFile = window.__TAURI__?.fs?.readTextFile;
  if (typeof readTextFile !== "function") {
    updateCapabilityLog("forbiddenRead", "fs.readTextFile unavailable");
    return;
  }
  try {
    const contents = await readTextFile(filePath);
    setFrontendState({
      lastPolicyError: null,
      lastFileRead: contents.slice(0, 80),
    });
    updateCapabilityLog(
      "forbiddenReadUnexpectedSuccess",
      contents.slice(0, 80),
    );
  } catch (error) {
    setFrontendState({ lastPolicyError: String(error) });
    updateCapabilityLog("forbiddenRead", String(error));
    updateStatusView();
  }
}

async function checkGuestMode() {
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  if (typeof invoke !== "function") {
    updateCapabilityLog("checkEnv", "bridge unavailable");
    return null;
  }
  try {
    const result = await invoke("check_env", {});
    const mode = result.ato_guest_mode ?? result.result?.ato_guest_mode ?? null;
    setFrontendState({
      lastGuestMode: mode,
    });
    applyModeLabel(mode);
    updateCapabilityLog("checkEnv", result);
    updateStatusView();
    return mode;
  } catch (error) {
    updateCapabilityLog("checkEnvError", String(error));
    return null;
  }
}

async function boot() {
  const bridgeDetected =
    typeof window.__TAURI_INTERNALS__?.invoke === "function";
  setFrontendState({ bridgeDetected });
  bridgeStatus.textContent = bridgeDetected ? "injected" : "missing";
  badgeLabel.textContent = bridgeDetected
    ? "Bridge injected by Desky"
    : "Waiting for bridge";

  const removeShellState = await window.__TAURI__?.event?.listen?.(
    "desky:shell-state",
    (payload) => {
      applyShellEvent("desky:shell-state", payload);
    },
  );
  const removeFocus = await window.__TAURI__?.event?.listen?.(
    "desky:tab-focus",
    (payload) => {
      applyShellEvent("desky:tab-focus", payload);
    },
  );
  const removePerm = await window.__TAURI__?.event?.listen?.(
    "desky:permission-granted",
    (payload) => {
      applyShellEvent("desky:permission-granted", payload);
    },
  );
  const removePolicy = await window.__TAURI__?.event?.listen?.(
    "desky:policy-rejected",
    (payload) => {
      applyShellEvent("desky:policy-rejected", payload);
      setFrontendState({
        lastPolicyError: payload?.detail ?? "policy rejected",
      });
      updateCapabilityLog("policyRejected", payload);
      updateStatusView();
    },
  );

  try {
    const session = await window.__DESKY_GUEST__?.session?.();
    setFrontendState({ sessionReady: Boolean(session) });
    sessionStatus.textContent = session?.sessionId ?? "ready";
    updateStatusView([JSON.stringify(session, null, 2)]);
  } catch (error) {
    sessionStatus.textContent = "failed";
    updateStatusView([`sessionError: ${String(error)}`]);
  }

  pingButton?.addEventListener("click", () => invokePing());
  setTitleButton?.addEventListener("click", () => setHostTitle());
  readFileButton?.addEventListener("click", () => readWorkspaceFile());
  forbiddenReadButton?.addEventListener("click", () => readOutsideBoundary());
  externalButton?.addEventListener("click", requestExternalOpen);

  if (bridgeDetected) {
    const guestMode = await checkGuestMode();
    if (guestMode === "1") {
      await setHostTitle();
      await readWorkspaceFile();
      await readOutsideBoundary();
    } else {
      updateCapabilityLog("mode", "standalone branch active");
      updateStatusView(["mode: standalone branch active"]);
    }
    await invokePing();
  }

  window.addEventListener("beforeunload", () => {
    removeShellState?.();
    removeFocus?.();
    removePerm?.();
    removePolicy?.();
  });
}

void boot();
