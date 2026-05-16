const POST = window.ipc?.postMessage
  ? (m) => window.ipc.postMessage(JSON.stringify(m))
  : (m) => console.log("[no bridge]", m);

export const BRIDGE = (command) => POST({ capsule: "ato-onboarding", command });
