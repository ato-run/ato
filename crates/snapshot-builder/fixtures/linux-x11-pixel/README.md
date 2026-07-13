# Linux X11 pixel-stream fixture

Deterministic-shape Dockerfile-import fixture for the authenticated
`ato.pixel-stream.v1` vertical slice. The image starts Xvfb, Openbox, one xterm
application, a build-readiness HTTP endpoint on `8080`, and a guest-private RFB
endpoint on `5900`.

The readiness gate correlates all four build signals before seal:

- the expected xterm PID is alive;
- a top-level window with `WM_CLASS=AtoPixelFixture` exists;
- that window is mapped and viewable;
- the root framebuffer hash changes after the target app is mapped.

Use the snapshot-builder `dockerfile_import` lane with `Dockerfile`,
`port_override=8080`, and `readiness_http_path=/health`. Register the trusted
surface requirement as `pixel_stream / ato.pixel-stream.v1`, and emit these
explicit restore endpoints:

```json
[
  {
    "role": "app_http",
    "protocol": "http",
    "exposure": "host_internal",
    "port": 8080,
    "readiness": { "kind": "http_get", "path": "/health" }
  },
  {
    "role": "pixel_rfb",
    "protocol": "tcp",
    "exposure": "guest_private",
    "port": 5900,
    "readiness": { "kind": "first_frame" }
  }
]
```

The RFB process has no password or session credential at build time. Direct
public access to `5900` is forbidden; after restore, the authenticated host
gateway is the only public path. Session readiness additionally requires the
host-side RFB probe to receive a complete framebuffer update.

MVP constraints are fixed at Linux x86_64, software rendering, 1280×720,
approximately 30 fps, and US keyboard input. Clipboard, file transfer, audio,
GPU, dynamic resize, accessibility projection, and IME are disabled.
