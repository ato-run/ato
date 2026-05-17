let requestCounter = 0;

function nextRequestId(prefix) {
  requestCounter += 1;
  return `${prefix}-${requestCounter}`;
}

function getPostMessage() {
  if (window.__ATO_IPC__ && typeof window.__ATO_IPC__.postMessage === "function") {
    return (message) => window.__ATO_IPC__.postMessage(JSON.stringify(message));
  }
  if (window.ipc && typeof window.ipc.postMessage === "function") {
    return (message) => window.ipc.postMessage(JSON.stringify(message));
  }
  return (message) => {
    console.log("[ato-dock bridge missing]", message);
    return false;
  };
}

const postMessage = getPostMessage();

export function postDockCommand(command) {
  return postMessage({ capsule: "ato-dock", command });
}

export function requestLogin() {
  return postDockCommand({ kind: "login", request_id: nextRequestId("login") });
}

/**
 * Open the GitHub Import review surface for `url`. Used when the dock
 * detects that a GitHub repository URL was entered and the user wants
 * to import-and-run it rather than register it as a developer-owned
 * capsule.
 */
export function postImportOpen(url) {
  return postMessage({ capsule: "ato-import", command: { kind: "open", url } });
}

/**
 * Recognize GitHub repository URLs in any of the accepted forms.
 * Mirrors `crate::source_import_session::normalize_github_import_input`
 * acceptance for the prefix-based forms; intentionally does not match
 * bare `owner/repo` (which is also accepted by the dock's manual
 * persist flow).
 */
export function looksLikeGitHubRepoUrl(input) {
  if (typeof input !== "string") return false;
  const trimmed = input.trim().toLowerCase();
  return (
    trimmed.startsWith("github.com/") ||
    trimmed.startsWith("www.github.com/") ||
    trimmed.startsWith("https://github.com/") ||
    trimmed.startsWith("https://www.github.com/") ||
    trimmed.startsWith("http://github.com/") ||
    trimmed.startsWith("http://www.github.com/")
  );
}
