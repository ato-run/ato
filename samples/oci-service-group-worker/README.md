# OCI service group with a background worker

Web + DB + queue + worker in one `ato run`. Three services serve a logical
Port; the worker serves none, so it declares `ports = []` (omitting the field
means the same thing).

```sh
ato pack samples/oci-service-group-worker --output /tmp/worker.capsule
ato run /tmp/worker.capsule --no-open
```

A service with no Port is a serving step like any other: it has its own image,
argv, environment, state and Bindings scoped to it. Its readiness is "still
running", because it has no socket to connect to; a service that exits during
startup still fails the group. The group still needs exactly one service
serving `app.http`.

The native process service group (`ato.process@1`) does not model a serving
step without a Port, because that Adapter proves a start by connecting to the
Port; such a manifest is refused at validation with a message naming the
service.
