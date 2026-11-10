//! Static app serving and the Data Ports bridge for the selfhost Coordinator.
//!
//! The bridge exposes the same `window.atoDataPorts` shape the hosted runtime
//! serves, so a sample app written against the hosted contract runs unchanged
//! against SQLite. The bridge is same-origin and never holds a secret: it calls
//! the Coordinator's own API from the served page.

use std::path::{Component, Path, PathBuf};

// The hosted runtime injects a context script before the app's scripts;
// selfhost must do the same or the app's client has nothing to call.
pub const DATA_PORTS_BRIDGE_PATH: &str = "/__ato/data-ports/bridge-v1.js";
pub const DATA_PORTS_BRIDGE_JS: &str = r#"(function(){
  if (window.atoDataPorts) return;
  var base = '/__ato/data-ports';
  function failure(code, status) {
    var error = new Error(code);
    error.name = 'AtoDataPortError';
    error.code = code;
    error.status = status || null;
    return error;
  }
  async function json(path, init) {
    var response = await fetch(path, Object.assign({ credentials: 'same-origin', cache: 'no-store' }, init));
    var value = await response.json();
    if (!response.ok) throw failure(value.error || 'data_port_failed', response.status);
    return value;
  }
  function requirements() { return json(base); }
  function request(port, operation, args, key) {
    if (!key) throw failure('idempotency_key_required');
    var body = JSON.stringify({ operation: operation, arguments: args, idempotency_key: key });
    var running = null;
    function run() {
      if (!running)
        running = json(base + '/' + encodeURIComponent(port) + '/invoke', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: body
        }).finally(function(){ running = null; });
      return running;
    }
    return Object.freeze({ port: port, operation: operation, invoke: run });
  }
  Object.defineProperty(window, 'atoDataPorts', {
    value: Object.freeze({
      requirements: requirements,
      request: request,
      invoke: function(port, operation, args, key) { return request(port, operation, args, key).invoke(); }
    })
  });
})();"#;

pub const RUNTIME_CONTEXT_PATH: &str = "/__ato/runtime-context.js";

/// The runtime context the hosted proxy injects into a served app: the
/// operation handle the app's client uses and the state-change subscription the
/// UI re-renders on. It is generated per app instance (the Port name is the
/// declaration, not a global).
pub fn runtime_context_js(port: &str) -> String {
    // The Port name comes from the operator's serve flags; keep only characters
    // that cannot break out of the generated string.
    let port: String = port
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
        .collect();
    format!(
        r#"(function(){{
  if (window.atoInstanceOperations) return;
  var port = '{port}';
  function invoke(operation, args, key) {{
    return window.atoDataPorts.invoke(port, operation, args, key);
  }}
  window.atoInstanceOperations = Object.freeze({{
    prepare: function(operation, args, key) {{
      return Object.freeze({{ operation: operation, invoke: function() {{ return invoke(operation, args, key); }} }});
    }},
    invoke: invoke
  }});
  var subscribers = [];
  window.__atoInstanceState = Object.freeze({{
    subscribe: function(callback) {{ subscribers.push(callback); return function() {{}}; }},
    notify: function() {{
      subscribers.forEach(function(callback) {{ try {{ callback(); }} catch (_) {{}} }});
    }}
  }});
  var bridgeInvoke = window.atoDataPorts.invoke;
  window.atoDataPorts.invoke = function(target, operation, args, key) {{
    return bridgeInvoke(target, operation, args, key).then(function(result) {{
      try {{ window.__atoInstanceState.notify(); }} catch (_) {{}}
      return result;
    }});
  }};
}})();"#
    )
}

/// Inject the bridge and the runtime context before the app's own scripts.
pub fn inject_runtime(html: &str) -> String {
    let tags = format!(
        r#"<script src="{}"></script><script src="{}"></script>"#,
        DATA_PORTS_BRIDGE_PATH, RUNTIME_CONTEXT_PATH
    );
    if let Some(index) = html.find("<script") {
        let mut injected = String::with_capacity(html.len() + tags.len());
        injected.push_str(&html[..index]);
        injected.push_str(&tags);
        injected.push_str(&html[index..]);
        injected
    } else if let Some(index) = html.rfind("</body>") {
        let mut injected = String::with_capacity(html.len() + tags.len());
        injected.push_str(&html[..index]);
        injected.push_str(&tags);
        injected.push_str(&html[index..]);
        injected
    } else {
        format!("{tags}{html}")
    }
}

/// Resolve a request path inside the app directory, refusing traversal and
/// absolute components. `/` maps to `index.html`.
pub fn resolve(apps_dir: &Path, request_path: &str) -> Option<PathBuf> {
    let relative = request_path.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    let mut path = apps_dir.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => path.push(part),
            _ => return None,
        }
    }
    if path.is_file() { Some(path) } else { None }
}

pub fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_traversal_and_serves_the_entry() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("index.html"), "<h1>app</h1>").unwrap();
        std::fs::write(directory.path().join("app.js"), "console.log(1)").unwrap();
        assert!(resolve(directory.path(), "/").is_some());
        assert!(resolve(directory.path(), "/app.js").is_some());
        assert!(resolve(directory.path(), "/../secret").is_none());
        assert!(resolve(directory.path(), "/%2e%2e/secret").is_none());
        assert!(resolve(directory.path(), "/nope.js").is_none());
        assert_eq!(
            content_type(Path::new("app.js")),
            "text/javascript; charset=utf-8"
        );
    }
}
