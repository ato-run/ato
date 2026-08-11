# Capsule Protocol PTY error fixture

This deliberately fails `rustc main.rs` with a type mismatch. It has no
network or registry dependency and is used to prove that a producer workspace
can be captured, removed, restored from one portable bundle, replayed with
actual PTY egress observation, and continued with new input.
