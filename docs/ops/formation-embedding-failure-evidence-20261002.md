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
precedence, and typed process rejection across a read boundary. Linux final-code
and real Coordinator/Runtime cause propagation remain pending.

The prior actual fixtures prove enforcement and the positive UI path, not
correct final-code negative classification. They are not counted as OSS app
successes or paid API provider calls. No external service authentication,
deployment, remote migration, old WBO replay, or 100-case measurement occurred.
