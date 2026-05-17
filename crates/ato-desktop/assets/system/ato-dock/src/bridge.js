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
