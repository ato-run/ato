# Candidate stop preservation — 2026-10-02

Temporary realization previously deleted scratch even when process termination
failed. Its subsequent Drop, seeing the consumed process handle, could also
interpret the missing handle as a confirmed stop. The Runtime now remembers
unconfirmed termination through repeated teardown/Drop and retains scratch.
Confirmed termination followed by scratch removal failure has a distinct typed
finding. Worker-level cleanup keeps the full attempt tree for unconfirmed stop
or unavailable/unfinished execution history.

The common StopClass is carried as an optional, bounded `candidate_stop` enum
on the existing attestation for both source and retained attempts. It contains
no private path/log/value. A finished execution record is preserved truthfully;
it cannot establish physical termination. Companion API changes hold the Search
as UNKNOWN and forbid new candidates and retention; K receipts remain evidence.
The draft contract is [Formation unconfirmed stop](../rfcs/draft/FORMATION_UNCONFIRMED_STOP.md).

Local validation: Runtime 89 and Worker 77 tests PASS; CLI/Worker/Runtime
all-target clippy PASS. Includes repeated teardown/Drop with preserved owned
state, full attempt-root preservation and private-context wire omission.
Existing same-K receipt tests remain passing. Linux Runtime 90 PASS / 1
existing ignored; Worker 78 PASS. Actual Coordinator/Runtime at execution pin
`7b8db6615f5df02798c4d49b00802f2dab950dee` and API
`0b1540069e3bd2da7866c661bbcc83f4942ceb24`: normal same-K PASS and expired
setup typed failure both persist result ACK and close delivery under original
retry policy, with confirmed cleanup and zero reservations. Both retain phase
timings and the optional confirmed-stop attestation through actual API routes.
Public [control metadata](evidence/formation-unconfirmed-stop-controls-20261002.json)
SHA-256 `88c0258b3518b1c1b0bcae3027502c4881a22bcdf2c159ec11c9bc0eedc54fda`.
Runtime code is `a1b4e93f`; the subsequent pin changes only the acceptance
Python harness. Earlier disk-guard pre-execution failures and a known port
contention failure from overlapping fixtures are preserved. The successful
normal control runs serially with identical Source/K/D. Only this task's unused
incremental compiler cache was removed; frozen binary hashes are unchanged.

A new owned actual disconnect case has stopped Coordinator communication during
setup and exhausted its original report retry while preserving the saved result.
It is waiting for the real unchanged 30-minute claimed-attempt timeout before
UNKNOWN observation. No Runtime/requester restart or inferred resolution is
performed; old WBO UNKNOWN is untouched. Physical-stop uncertainty remains a
separate injected component boundary, not completed physical fault acceptance.

The small-pilot harness also waits up to 60 seconds for durable result settlement
after requester timeout, keeping the original provisional status and writing a
separate final view. It starts no new requester, inference or execution and
does not reset execution limits. An exhausted report retry remains exhausted.

Compiler/authority unchanged at `b7c322e70d28a986581e6f60fb6d716309e5a6c8`.
No remote migration, deployment, ordinary Run authorization, paid API call or
old UNKNOWN replay. API schema support must precede Worker deployment.
