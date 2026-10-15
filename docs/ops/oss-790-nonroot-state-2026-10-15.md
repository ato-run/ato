# Writable OCI state admission (#790 / API #783)

A writable mount previously selected its host directory's UID:GID verbatim for
`docker run --user`. A directory provisioned as root therefore selected root in
the workload. Admission now refuses UID0 or GID0 with typed
`OciWritableUserUnauthorized` / `oci_writable_user_unauthorized` before querying or
pulling the image. Run arguments check again before container launch. A valid,
single nonroot owner is preserved; mixed owners remain refused. No ownership,
permission, network, capability or resource limit is changed.

The change does not implement arbitrary UID mapping, privileged initialization,
stateless image-user admission or full OCI curation acceptance. Memos's requested
10001:10001 and Mealie's911:911 still need their own approved state-binding
acceptance; a shared Runner's1000:1000 is not reported as those fixtures passing.
No shared Runner binary/service, Docker storage, flag or live application changed.

## Validation

- OCI package33 unit tests passed;3 native Linux Docker integration tests remain
  ignored on macOS. Root/mixed-owner refusal and refusal before the fake Docker
  command are tested; nonroot ownership still projects unchanged into `--user`.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --locked --workspace --all-targets --all-features -j2 -- -D warnings`
  passed using this task's separate target directory.
- Final `cargo test --locked --workspace --no-fail-fast -j2 -- --test-threads=1`
  passed1964 tests, failed0, ignored9, including documentation tests. Fresh ATO_HOME,
  XDG directories and TMPDIR were all under the workspace's `.tmp` directory.
- The first parallel run aborted in an unchanged HTTP preparation test after a
  WouldBlock/startup timeout; the untouched base's162 runtime-attempt tests passed.
  An initial serial run exposed the harness's too-long Unix socket paths. The
  netd targets passed after selecting a shorter workspace-local temporary root,
  then the final entire workspace run above passed. No test expectation or skip
  was changed to obtain that result.

Base00d6834862df65d1ef6a10b21c04ce8d4ec6bdf2 contains5 inherited commits ahead
of cached origin/main. Review this fix against that pinned input, not the unrelated
nightly ancestry. Those build-toolchain changes remain integration dependencies.


## Numeric principal observations

The existing container startup network inspection now also returns its exact ID
and init PID, without an extra Docker call. Bounded Linux procfs reads verify the
container cgroup, identity UID/GID maps and consistent process start ticks, then
cache optional startup credentials. Remote daemons, remapping, non-Linux and missing
or contradictory reads yield unknown evidence. Connected Runner reports the same
object per container in existing lease execution metadata. Root may be reported
as a fact; the observer does not grant execution or judge K/D. Design constraints
are in draft/OCI_EXECUTION_PRINCIPAL_EVIDENCE.md. No runtime permission or storage
ownership is changed.

OCI35 tests passed on macOS; the updated full workspace passed1966 tests with0
failures and9 existing environment-dependent ignores. Workspace all-feature/
all-target clippy passed after boxing the optional cached observation to preserve
the existing handle/enum size. Native Linux code was cross-compiled offline and
its read-only kernel reader passed against one explicitly selected already running
managed container. Its1000:1000 observation is not Memos10001 or Mealie911 acceptance.
No container or shared Runner was created, stopped, modified or deployed for it.
The bounded public smoke record omits the existing container ID/PID; those remain
in the private operational artifact. Actual startup collection on a new approved
curation Run and startup performance still require staging acceptance.
