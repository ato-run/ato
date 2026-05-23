# Per-Target Readiness Timing Controls

Ato supports per-target readiness timing so that slow apps can declare longer
timeouts without affecting failure detection speed for fast apps.

## Fields

All three fields are optional on any `readiness_probe` block.

| Field | Type | Default | Description |
|---|---|---|---|
| `initial_delay_seconds` | `u32` | `0` | Seconds to wait before the first probe attempt |
| `timeout_seconds` | `u32` | `180` | Total seconds before the probe is considered failed. Must be `> 0`. |
| `interval_seconds` | `u32` | `2` | Seconds between successive probe attempts. Must be `> 0`. |

`initial_delay_seconds` must be strictly less than `timeout_seconds`.

## Supported probe types

Timing fields work with all three probe types.

### HTTP probe

```toml
readiness_probe = { http_get = "/health", port = "3000", timeout_seconds = 420 }
```

### TCP probe

```toml
[services.db.readiness_probe]
tcp_connect = "5432"
port = "5432"
initial_delay_seconds = 5
timeout_seconds = 120
interval_seconds = 2
```

### Exec probe

```toml
[services.db.readiness_probe]
exec = ["pg_isready", "-U", "postgres"]
port = "5432"
initial_delay_seconds = 5
timeout_seconds = 120
interval_seconds = 2
```

## When to use `initial_delay_seconds` vs `timeout_seconds`

- **`initial_delay_seconds`** — use when the app is known to need startup time
  before it will ever accept connections (e.g., DB initialisation). Avoids
  noisy early failures in logs.
- **`timeout_seconds`** — use when the app might take a long time on first run
  (cold image pull, DB migration, model download). Does **not** skip early
  probing.

> **Warning:** do not use a long `timeout_seconds` to mask a wrong readiness
> path. If the endpoint never returns 200, extending the timeout only delays
> the failure. Fix the probe path first.

## Validation rules

Ato rejects the following at recipe parse time:

- `timeout_seconds = 0` — must be `> 0`
- `interval_seconds = 0` — must be `> 0`
- `initial_delay_seconds >= timeout_seconds` — delay must be strictly less than
  timeout

## Execution semantics

For each readiness probe:

1. Wait `initial_delay_seconds` before the first attempt.
2. Poll every `interval_seconds`.
3. Fail after `timeout_seconds` total (counting from step 1).
4. On timeout, Ato reports the target name and the configured timeout in the
   error message.

For `depends_on` edges: a dependent service does not start until its dependency
has satisfied its own readiness timing — each dependency uses its own probe
configuration, not a global timeout.

## Identity

Readiness timing values are included in the execution identity of a recipe.
Changing `timeout_seconds` or `interval_seconds` produces a different identity,
the same as changing the image or port.

## Examples

### Open WebUI (first-run HuggingFace model download)

Open WebUI downloads embedding models on first run, which can take several
minutes. Because the URL is available immediately, no probe is set; the recipe
documents the expected startup behaviour instead.

### Langflow (Python init + DB migration)

```toml
readiness_probe = { http_get = "/health", port = "7860", timeout_seconds = 420 }
```

### Postgres (exec probe with initial delay)

```toml
[services.db.readiness_probe]
exec = ["pg_isready", "-U", "postgres"]
port = "5432"
initial_delay_seconds = 5
timeout_seconds = 120
interval_seconds = 2
```

### AnythingLLM (model workspace init)

```toml
readiness_probe = { http_get = "/", port = "3001", timeout_seconds = 300 }
```

## Defaults are conservative

The default `timeout_seconds = 180` matches the previous global constant.
Fast apps retain fast failure detection. Slow apps opt in to longer timeouts
explicitly.
