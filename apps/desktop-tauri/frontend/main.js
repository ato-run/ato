// Minimal Ato desktop shell frontend.
//
// The bundled Launcher delegates every Capsule operation to the `ato` CLI
// through typed Tauri commands. It never reads `.capsule/` or advances a
// Computation itself.

const { invoke } = window.__TAURI__.core;

const project = document.getElementById("project");
const status = document.getElementById("status");
const output = document.getElementById("output");
const openSurface = document.getElementById("open-surface");

function log(text) {
  output.textContent += text + "\n";
}

function clear() {
  output.textContent = "";
}

function projectValue() {
  const value = project.value.trim();
  if (!value) {
    log("error: enter a project path");
    return null;
  }
  return value;
}

async function refreshStatus() {
  const value = projectValue();
  if (!value) return;
  try {
    const view = await invoke("run_inspect", { project: value });
    const head = view.head ? view.head : "-";
    status.textContent = `${view.branch || "-"} · ${view.status} · ${head}`;
    openSurface.disabled = !(view.status === "active" && view.surfaces.length === 1);
  } catch (error) {
    status.textContent = "inspect failed";
    log(`error: ${error}`);
  }
}

async function execute(command) {
  const value = projectValue();
  if (!value) return;
  clear();
  log(`running ${command.kind} …`);
  try {
    const result = await invoke("computation_execute", { command });
    log(result.success ? result.output : `failed: ${result.output}`);
    await refreshStatus();
  } catch (error) {
    log(`error: ${error}`);
  }
}

document.getElementById("browse").addEventListener("click", async () => {
  try {
    const selected = await invoke("pick_project");
    if (selected) project.value = selected;
  } catch (error) {
    log(`error: ${error}`);
  }
});

document.getElementById("init").addEventListener("click", () =>
  execute({ command: "init", capsule: projectValue(), initial_only: false })
);
document.getElementById("resume").addEventListener("click", () =>
  execute({ command: "resume", selector: projectValue(), branch: null })
);
document.getElementById("stop").addEventListener("click", () =>
  execute({ command: "stop", capsule: projectValue() })
);
document.getElementById("encap").addEventListener("click", () =>
  execute({
    command: "encap",
    selector: projectValue(),
    output: "computation.capsule",
  })
);
document.getElementById("run").addEventListener("click", async () => {
  const capsule = projectValue();
  if (!capsule) return;
  clear();
  log("running capsule … (Cancel stops the CLI process tree)");
  const run = document.getElementById("run");
  const cancel = document.getElementById("cancel");
  run.disabled = true;
  cancel.disabled = false;
  try {
    const result = await invoke("computation_execute", {
      command: { command: "run_portable", capsule_file: capsule },
    });
    log(result.success ? result.output : `failed: ${result.output}`);
  } catch (error) {
    log(`error: ${error}`);
  } finally {
    run.disabled = false;
    cancel.disabled = true;
  }
});
document.getElementById("cancel").addEventListener("click", async () => {
  try {
    await invoke("run_cancel");
    log("cancel requested");
  } catch (error) {
    log(`error: ${error}`);
  }
});
document.getElementById("open-surface").addEventListener("click", async () => {
  const value = projectValue();
  if (!value) return;
  try {
    await invoke("open_web_surface", { project: value });
  } catch (error) {
    log(`error: ${error}`);
  }
});

invoke("desktop_info")
  .then((info) => {
    document.getElementById("shell-info").textContent =
      `Ato Desktop ${info.version} · ${info.platform}`;
  })
  .catch(() => {});

refreshStatus();
