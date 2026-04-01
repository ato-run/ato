# Simple TODO App - Capsule Deployment

This directory contains a Simple TODO application ready to be deployed as a Capsule.

## Features

- ✅ Minimal React/Vite application
- 📦 Capsule-compatible packaging
- 🚀 Python-based runtime entry point
- 🔒 Secure container deployment
- 💾 Task management with local storage

## Project Structure

```
simple-todo/
├── capsule.toml          # Capsule manifest (UARC V1.0 compliant)
├── app.py                # Python runtime entry point
├── dist/                 # Built React application (generated)
└── README.md             # This file
```

## Building the Capsule

### Prerequisites

- Node.js and pnpm (for building the React app)
- Python 3.9+ (for the runtime)
- ato CLI tools (optional, for capsule verification)

### Build Steps

1. **Build the React application** (from `ato-desktop/`):

```bash
cd ../../
pnpm run build
# Output will be in dist/ directory
```

2. **Package as a Capsule**:

```bash
# Using the ato CLI
ato pack --bundle --manifest ./capsule.toml

# Or build a signed package
ato pack --bundle --manifest ./capsule.toml --key ./keys/signing.key
```

## Running the Capsule

### Local Development

```bash
# Run the Python server directly
python3 app.py

# Server starts on http://localhost:8000
# Health check: http://localhost:8000/api/health
```

### In Capsule Runtime

```bash
# Deploy to a capsule runtime
ato open simple-todo-v0.1.0.capsule

# Verify deployment
curl http://localhost:8000/api/health
```

## API Reference

### Health Check

```http
GET /api/health

Response (200):
{
  "status": "ok",
  "service": "simple-todo",
  "version": "0.1.0"
}
```

### Create TODO

```http
POST /api/todos
Content-Type: application/json

{
  "title": "Learn Capsules"
}

Response (201):
{
  "id": "1234567890",
  "title": "Learn Capsules",
  "completed": false
}
```

## Deployment to Capsule Network

1. **Package the capsule**:
   ```bash
   ato pack --bundle --manifest ./capsule.toml --output simple-todo.capsule
   ```

2. **Sign the package**:
   ```bash
   ato pack --bundle --manifest ./capsule.toml --key ./my-signing-key
   ```

3. **Deploy to coordinator**:
   ```bash
   gumball-cli deploy simple-todo.capsule --coordinator http://coordinator:8081
   ```

4. **Verify deployment**:
   ```bash
   gumball-cli status simple-todo
   ```

## Technical Details

### Capsule Configuration

- **Runtime**: Source (Python)
- **Language**: Python 3.9+
- **Entry Point**: `app.py`
- **Isolation**: Sandboxed with limited filesystem access
- **Network**: Egress allowed to localhost (configurable)

### Security

- Capsule signature verification
- Restricted filesystem mounts
- Network egress control
- Process isolation via container runtime

## Development

### Environment Variables

- `APP_DIR`: Working directory for the application (default: `/app`)
- `CAPSULE_HOST`: Server listen address (default: `0.0.0.0`)
- `CAPSULE_PORT`: Server listen port (default: `8000`)

### Testing the API

```bash
# Health check
curl http://localhost:8000/api/health

# Create a todo
curl -X POST http://localhost:8000/api/todos \
  -H "Content-Type: application/json" \
  -d '{"title":"Test task"}'
```

## Future Enhancements

- [ ] Database backend integration
- [ ] Authentication and user isolation
- [ ] Real-time updates via WebSocket
- [ ] Data persistence
- [ ] Container image (OCI) runtime support
- [ ] WebAssembly (Wasm) runtime target

## References

- [UARC Specification](../../uarc/SPEC.md)
- [nacelle Runtime](../README.md)
- [ato CLI](../../ato-cli/README.md)
- [React + Vite](../../ato-desktop/README.md)
