# Capsule Bundle

Status: Accepted

This RFC adopts Object Closure Bundle version 2 as the portable `.capsule`
format. `root` is logical Capsule identity. Each Materialization entry contains
only its versioned Materializer ID and descriptor ContentRef; implementation
details remain inside the descriptor.

`encap` resolves one selector, asks every selected Materializer to encode and
verify, traverses computation and materialization closures, then atomically
writes one canonical bundle. Partial success is not supported.

`run` verifies into temporary runtime storage, deterministically selects a
compatible restore-capable Materialization, restores, checks Bindings, and
starts an ephemeral Run. It never advances an authored branch.
