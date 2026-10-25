# Bounded immutable inputs through the Data Plane

Status: draft implementation contract
Date: 2026-10-04

Portable authoring v0 accepts up to two `ato.model-set@1` inputs beside the
required workspace. Each independently pins a canonical manifest by digest;
both use the existing BoundInput, closure validator, Data Grant, Runner cache,
and read-only materialization. There is no new semantic kind for software.
The existing protocol name also covers hash-pinned software archives and
wheels, so model and software manifests can evolve independently. Their
objects remain outside the portable bundle. Source URLs, package labels and
provider placement stay outside D.

An input ID must be unique after the process adapter's uppercase/underscore
environment mapping. `workspace` and the Runner's Asset directory `assets`
are reserved. A grant with an unsafe or colliding input path fails before
materialization. Existing one-input bundles and their identities are unchanged.

Runner object evidence adds `bytes_requested` (sum of attempted ranges),
`bytes_transferred` (all bytes read, including failed partial responses), and
`verification_millis` (marker checks and full-file SHA-256 time). `millis`
continues to measure total delivery. Cache hits request and transfer zero
object bytes. The Coordinator accepts the new fields optionally so old Runner
reports remain valid; rollout deploys that compatibility before the new Runner.

The first consumer is staging Wan Animation: one nine-object model manifest
and a separate hash-locked software manifest. Python remains a pinned process
runtime requirement. Dependencies install offline in the Run's existing
scratch directory; no network permission is added to the workload. Both cold
delivery and warm reuse must be measured on real Runs, followed by a fresh
machine and output-save failure acceptance. These are execution gates, not
claims established by the unit tests.
