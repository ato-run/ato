const result = document.getElementById("result");
const allowlist = document.getElementById("allowlist");
const host = window.__ATO_HOST__;

allowlist.textContent = host ? host.allowlist.join(", ") : "no host bridge found";

function show(value) {
  result.textContent = typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

document.getElementById("probe").addEventListener("click", async () => {
  try {
    const response = await host.invoke("shell.workspaceInfo", "workspace-info", {
      route: window.location.href,
      workspace: "Rust host",
    });
    show(response);
  } catch (error) {
    show(error.message);
  }
});

document.getElementById("deny").addEventListener("click", async () => {
  try {
    const response = await host.invoke("shell.openExternal", "open-external", {
      url: "https://example.com",
    });
    show(response);
  } catch (error) {
    show(error.message);
  }
});