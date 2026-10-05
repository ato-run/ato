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

The new owned actual Coordinator disconnect during setup completed after the
original real claimed-attempt timeout (1804.144 seconds). The same Search
`search_108ca6b4d1bee703d37d472018241316` remains UNKNOWN with
`result_not_received`. Its saved Runtime result reports confirmed local cleanup,
but is not a Coordinator-delivered finding and cannot resolve UNKNOWN.
Original retry=1 exhausted dispatches 0 and 1, no ACK. Runtime/requester were
not restarted; a read-only observer restarted the same Coordinator only after
the original 30-minute timeout. One attempt, no extra execution/inference,
original deadline/caps/round and producer journal hashes unchanged.
Reservations are zero; UNKNOWN conservatively consumed the originally reserved
expanded/stored caps (536870912 bytes each), without automatic refund.
Public [disconnect metadata](evidence/formation-actual-disconnect-20261002.json)
SHA-256 `e5cf3e3bb336008a61ba3eaf06711b8fef078362e3fe7b86053bddd6bebc6641`. This new UNKNOWN and old WBO UNKNOWN are preserved.
Actual network disconnection is distinct from injected physical-stop uncertainty;
the latter still requires a separately recorded physical fault acceptance.

The small-pilot harness also waits up to 60 seconds for durable result settlement
after requester timeout, keeping the original provisional status and writing a
separate final view. It starts no new requester, inference or execution and
does not reset execution limits. An exhausted report retry remains exhausted.

Compiler/authority unchanged at `b7c322e70d28a986581e6f60fb6d716309e5a6c8`.
No remote migration, deployment, ordinary Run authorization, paid API call or
old UNKNOWN replay. API schema support must precede Worker deployment.
