# Harper browser Derivation

Minimal browser UI using the unmodified `harper.js` 2.13.0 WASM package.
The npm artifact and inspected upstream Git commit are independently pinned
in `provenance.json`; this sample does not assert that npm supplied a build
of that exact Git commit. Apache-2.0 upstream license is included.

From this directory:

```sh
npm ci --ignore-scripts
npm run build
python3 -m http.server 8767 --bind 127.0.0.1 --directory dist
# In a second terminal, with real Google Chrome installed:
npm run verify
```

The verifier uses real Chrome with its sandbox enabled. Its disposable browser
profiles do not receive account credentials. Evidence, screenshot and downloaded
text are saved under `.tmp/browser/` (ignored by Git). It checks correction,
save/reload, exact downloaded bytes, offline correction, outgoing text in HTTP
and WebSocket messages, and isolation from a fresh browser context.

Documents are saved explicitly in IndexedDB. No document is written to
localStorage because an embedding host can synchronize that storage remotely.
The unmodified WASM is downloaded once, then reused through a browser-owned
Blob URL by Harper's UI serializer and Worker. This avoids two simultaneous
network/cache writes for the same large immutable resource. CSP permits only
same-origin and browser-owned Blob connections; external analytics are not
allowed by this document.

`dist/capsule.toml` is a static-browser Formation input. Its current Contract
checks HTTP and workspace identity **only**. `browser-acceptance.txt` declares
the additional task before execution. `verify.mjs` returns browser evidence,
not a Rust verification receipt and not Discover acceptance. Publication must
wait until the same Rust authority binds that browser task, exact source,
Derivation and actual Run attempt into K and verifies every criterion.

Browser-local persistence does not demonstrate cross-device persistence or
independent Hosted Run copying. The initial staging archive used an earlier
localStorage implementation; this revision is not that published materialization.
See `docs/ops/oss-seven-20261023.md` for precise results and gates.
