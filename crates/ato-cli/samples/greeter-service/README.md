# Greeter — IPC Sample

Minimal example of **Capsule IPC** using a shared service and a client.

## Structure

```
samples/
  greeter-service/   ← Shared Service (exports "greet" and "health")
  greeter-client/    ← Client (imports from greeter-service)
```

## Quick Start

```bash
# 1. Register the service
ato ipc start samples/greeter-service

# 2. Check it's running
ato ipc status

# 3. Run the client (auto-imports greeter via IPC)
ato open samples/greeter-client

# 4. When done, stop the service
ato ipc stop --name greeter
```

## How it works

### greeter-service (`capsule.toml`)

```toml
[ipc.exports]
name = "greeter"
protocol = "jsonrpc-2.0"
transport = "unix-socket"

[ipc.exports.sharing]
mode = "singleton"       # One instance shared by all clients
idle_timeout = 30        # Auto-stop after 30s of inactivity

[[ipc.exports.methods]]
name = "greet"           # Available capability
```

### greeter-client (`capsule.toml`)

```toml
[ipc.imports.greeter]
from = "greeter-service"
activation = "eager"     # Start service before client
required = true          # Fail if service unavailable
```

### Communication flow

```
Client                    Broker                  Service
  |                         |                        |
  |-- ato open -------->|                        |
  |                         |-- start greeter ------>|
  |                         |<-- ready (socket) -----|
  |<-- CAPSULE_IPC_* env ---|                        |
  |                         |                        |
  |-- greet({name}) ------- JSON-RPC 2.0 ---------->|
  |<-- {greeting} --------- JSON-RPC 2.0 -----------|
  |                         |                        |
  |-- exit ---------------->|                        |
  |                         |-- ref_count=0 -------->|
  |                         |-- idle_timeout(30s) -->|
  |                         |-- SIGTERM ------------>|
```

## Validation

```bash
# Validate the service manifest
ato validate samples/greeter-service

# Validate the client manifest
ato validate samples/greeter-client
```
