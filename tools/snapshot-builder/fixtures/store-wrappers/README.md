# Store wrapper recipes (login-bypass preview builds)

Reusable pattern for Store capsules whose upstream image requires an
interactive login that the anonymous free-preview lane cannot satisfy
(cookie/JWT sessions, seeded admin credentials, etc.).

Each `<slug>/` directory is the **recipe root** for one capsule:

```
<slug>/
  Dockerfile      # FROM <upstream image>@sha256:<digest>; COPY config + demo content
  config.yaml     # app-native auth-bypass config (noauth / anonymous mode)
  demo/           # small, secret-free sample content baked into the image
```

Rules:

- **FROM must be digest-pinned** — a tag is not a reproducible identity
  (the importer rejects unresolvable refs anyway).
- **No secrets anywhere** — the content is public in git and scanned by the
  no-secret seal gate. Auth bypass must use the app's own anonymous/noauth
  mode, never a baked credential.
- Auth bypass is for the **throwaway preview lane** (tmpfs-backed, time-boxed
  runs). Authenticated durable runs should keep upstream auth; that is a
  separate target/recipe when needed.
- Registration: `capsule_source_recipes` row pins `ato-run/ato@<commit>` with
  `subdirectory` = this recipe dir; the snapshot job uses
  `kind=dockerfile_import`, `dockerfile_path="Dockerfile"`, plus whatever
  `ephemeral_mounts` the app needs for writable paths.

First user: `filebrowser-quantum/` (FileBrowser Quantum `auth.methods.noauth`).
