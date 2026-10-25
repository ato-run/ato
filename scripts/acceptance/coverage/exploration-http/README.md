# Exploration HTTP acceptance workload

Start `server.mjs` with Node 22.14.0. It listens on
`ATO_ENDPOINT_APP_HTTP_PORT` or port 8080 and serves `hello\n` at GET `/`.
The exact `cookie` dependency is frozen in package.json and package-lock.json.
Install it with npm ci before startup. Dependency retrieval needs HTTPS to
registry.npmjs.org. Runtime needs only the logical app.http bind authority;
no runtime egress or persistent state is required.

This is an infrastructure acceptance workload, not an upstream OSS coverage
or functional-acceptance result. It uses the actual contained process adapter,
Coordinator and Verifier. Source remains immutable throughout every round.
