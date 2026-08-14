# Core architecture

The semantic center consists of three libraries:

- `ato-computation` defines canonical semantic values and identity.
- `ato-kernel` advances `ComputationRef` values through registered semantics.
- `ato-objects` persists and transports verified object closures.

`ato-ipc` is adjacent process wire, not a semantic protocol model.

Concrete behavior belongs to semantics extensions. External syntax belongs to
adapters. Physical execution, security enforcement, secrets, networking, and
snapshot capture belong to providers or services. Trace/receipt storage is an
optional transition observer and does not affect identity.

The enforced dependency direction is:

```text
computation
    ▲
objects
    ▲
kernel          ipc
    ▲             ▲
semantics      services
    ▲
adapters / providers
    ▲
apps / tools
```

Run `cargo run -p arch-check`; it validates the actual graph returned by
`cargo metadata`, including forbidden legacy package names.
