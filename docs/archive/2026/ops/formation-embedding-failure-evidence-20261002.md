# Formation artifact embedding failure evidence, 2026-10-02

This change follows Ato #1461. It preserves the existing static bundle producer
and process artifact guard; it does not add a second scan or relax permissions.

## Observed defect

With API `4af64035203e767088edf70bcbb5ab42c9c38b3b`, receipt authority built
from Ato `5143a6dcbdad02c340a050e01906d16ae2a6ded9`, PWA
`c549ae997b5db60eab0d6a7f13ae56b1df284518` and Runtime built from that Ato
source, a source-owned Node build embedded non-secret configuration supplied
through a private build-phase variable grant. Both fixtures started from the
same source closure and frozen K, without a known D.

* Search `search_5a78bda2867ec6ba6d1b5b769450d5e0`: registration without
  embedding permission returned HTTP 409. The owner selected permission in the
  actual PWA, registration returned 200, and attempt
  `01M3WWQ4NW5A0YED1MTYQTFBMX` reached fresh K PASS and submitted a retained D
  awaiting assessment. No ordinary Run permission was issued.
* Search `search_097139635234a25ce027026830daccb2`: registration without
  permission succeeded for a requirement declaring no embedding. The build
  exited successfully, but the canonical static producer rejected its output
  before publication. Attempt `01M3WWV17Q9JPFE009MPWVY9H2` did not publish an
  artifact. Its requester/common reasoning input lost the cause and exposed
  generic `formation_failed`. The producer's terminal infrastructure decline
  reflects that missing detail; it is not evidence that the guard was absent.

An earlier fixture, `search_1679547bd841143dc2c438a70a0d676d`, failed before
the source build because its owner-local sandbox could not traverse an ancestor
of the fixture toolchain path. It is preserved and excluded from embedding
acceptance. The corrected owner-local runtime used identical frozen binaries
in its own `/opt/ato/.tmp` directory; sandbox/network permissions were retained.

## Change and validation

The canonical materializer exports a value-free, path-free
`ArtifactEmbeddingRefused` error. Both canonical static output checks and the
existing bounded process scan return that type. `failure_of` recovers its stable
`secret_artifact_embedding_refused` code and a fixed build-stage explanation
through context. Existing `FormationFailure` precedence is retained, including
unfinished durable execution records. Untyped errors remain anonymous.

Local macOS validation: all 30 static materializer and 77 Runtime attempt unit
tests PASS. New cases cover blob and manifest rejection without partial output,
explicitly allowed output, preservation through private context, durable-error
precedence, and typed process rejection across a read boundary. At execution pin
`7d0822a1c7e6508c97a204f18bb26a3442c50139`, Linux passed 30 materializer
and 78 Runtime attempt tests, with one existing ignored test.

## Actual final-code input observations

* Search `search_f071722028c022691490161d1c1a746b`, attempt
  `01M3WXR3SHYJNC8YBQK4S7DZV1`: source build succeeded; the canonical guard
  refused output. The next common reasoning input contained
  `secret_artifact_embedding_refused`, without a value or internal path, and
  the producer returned `needs_input`. One execution, two rounds, no retained
  submission, zero outstanding byte reservations at terminal reporting.
* Search `search_b41c7cc4a1218d94021a1da359824f38`: two independently registered
  reusable values in the same source/resource scope produced `ambiguous_scope`
  and two metadata-only candidates before source execution. Owner selection
  used only the credential ID, preserving creation time, Search deadline and
  attempts used. The original candidate then ran once, was refused by the
  same embedding guard, and stopped as `needs_input`; no duplicate execution.
* Search `search_62e0287ddecfecb20645f696f32c3cda`: a reusable value with a
  separately scoped resource expired after its 15-second TTL. The common input
  view returned `expired` and zero selectable values; no source execution was
  permitted. Resume after the original round deadline recorded
  `round_deadline_exceeded` at Source admission without starting the build.
  Visible expired/revoked metadata led to `needs_input` at
  `2026-10-02T00:08:09.965Z`, with zero outstanding byte reservations. Original
  Search/round deadlines were not extended. Result SHA-256:
  `95934585357a1ca938bea352cae0511f3473b8a822cd36fc5fbde6e31148af5b`.

Owned reusable metadata from these tests was revoked through the normal API.
The final observed encrypted store contained 10 metadata rows, nine assignments,
zero live metadata rows and zero value rows. A scan of 215 public input/journal/
report files found zero occurrences of the private configuration. The
explicitly permitted embedded artifact is excluded from that scan; permission
does not authorize disclosure in model input or traces.

One attempted resume used an unsupported CLI argument and exited before any
network action. Its diagnostic is retained and excluded. Correct resumption
repeated the original command/configuration/journal, without changing counters.

The prior actual fixtures prove enforcement and the positive UI path, not
correct final-code negative classification. They are not counted as OSS app
successes or paid API provider calls. No external service authentication,
deployment, remote migration, old WBO replay, or 100-case measurement occurred.
