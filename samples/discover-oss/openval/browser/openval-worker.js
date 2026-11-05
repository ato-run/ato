/* The pinned Python runtime, wheels and engine are served from this origin. */
importScripts("/pyodide/pyodide.js");

let runtimePromise;
let pending = Promise.resolve();
const cancelled = new Set();

async function fetchBytes(path) {
  const response = await fetch(path);
  if (!response.ok) throw new Error("Required application files are unavailable.");
  return new Uint8Array(await response.arrayBuffer());
}

async function initialize() {
  const py = await loadPyodide({ indexURL: "/pyodide/" });
  await py.loadPackage(["pandas", "pydantic", "xlrd"]);
  const site = py.runPython("import site; site.getsitepackages()[0]");
  for (const wheel of [
    "numpy_financial-1.0.0-py3-none-any.whl",
    "et_xmlfile-2.0.0-py3-none-any.whl",
    "openpyxl-3.1.5-py2.py3-none-any.whl",
  ]) {
    py.unpackArchive(await fetchBytes(`/pyodide/${wheel}`), "zip", { extractDir: site });
  }
  py.FS.mkdirTree("/app");
  py.FS.mkdirTree("/work");
  py.unpackArchive(await fetchBytes("/openval-engine.zip"), "zip", { extractDir: "/app" });
  py.runPython("import sys; sys.path.insert(0, '/app'); import engine");
  return py;
}

async function execute(message) {
  if (cancelled.delete(message.id)) return;
  const py = await (runtimePromise ??= initialize());
  if (cancelled.delete(message.id)) return;
  let result;
  if (message.kind === "cashflow") {
    py.globals.set("_payload_json", JSON.stringify(message.payload));
    py.globals.set("_frequency", message.frequency);
    try {
      result = JSON.parse(py.runPython("engine.cashflow(_payload_json, _frequency)"));
    } finally {
      py.globals.delete("_payload_json");
      py.globals.delete("_frequency");
    }
  } else if (message.kind === "parse") {
    if (!(message.bytes instanceof ArrayBuffer) || message.bytes.byteLength > 10 * 1024 * 1024) {
      throw new Error("Workbook must be at most 10 MiB.");
    }
    const path = "/work/input.xlsx";
    py.FS.writeFile(path, new Uint8Array(message.bytes));
    try {
      result = JSON.parse(py.runPython("engine.parse_workbook('/work/input.xlsx')"));
    } finally {
      py.FS.unlink(path);
    }
  } else if (message.kind === "export") {
    const path = "/work/output.xlsx";
    py.globals.set("_payload_json", JSON.stringify(message.payload));
    try {
      py.runPython("engine.export_workbook(_payload_json, '/work/output.xlsx')");
      result = py.FS.readFile(path).buffer;
    } finally {
      py.globals.delete("_payload_json");
      try { py.FS.unlink(path); } catch { /* No output was created. */ }
    }
  } else {
    throw new Error("Unsupported operation.");
  }
  if (!cancelled.delete(message.id)) {
    if (result instanceof ArrayBuffer) self.postMessage({ id: message.id, result }, [result]);
    else self.postMessage({ id: message.id, result });
  }
}

self.onmessage = (event) => {
  const message = event.data;
  if (message.kind === "cancel") {
    cancelled.add(message.id);
    if (cancelled.size > 1000) cancelled.delete(cancelled.values().next().value);
    return;
  }
  pending = pending.then(() => execute(message)).catch((error) => {
    if (!cancelled.delete(message.id)) {
      self.postMessage({ id: message.id, error: String(error?.message ?? error) });
    }
  });
};
