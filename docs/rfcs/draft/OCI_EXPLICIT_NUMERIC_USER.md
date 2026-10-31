# Explicit nonroot OCI execution identity (draft)

Status: Draft.

The existing OCI Adapter chooses an execution UID:GID from writable state mount
ownership. A stateless image has no such selection. A route may now declare the
optional Adapter runtime fact `oci.user = "UID:GID"`, with each integer in
1..4294967294. Usernames, root, negative/fractional/unmapped IDs and extra components
are refused. This is the container's numeric identity, not an account credential,
host permission grant or Contract identity. It participates in D, never K.

The portable Rust validator accepts this known runtime fact for a single OCI
route or each service. The CLI projects it directly into the existing OciSpec.
Hosted launch projects the same numbers into optional user fields on the existing
OCI realization/service DTO. Existing fields and serialized input bytes remain
unchanged when omitted. Updated producers require runtime_feature=oci_user_v1;
old Runners are not chosen for a declared user, and no other D is substituted.
The decoder still rejects unknown fields. No new protocol root or migration is
introduced; existing golden fixtures remain unchanged and new fixtures pin both
language implementations.

The common Adapter validates the requested identity before Docker image admission
and in argument generation. It adds only `--user UID:GID`. If any writable state
mount exists, its already-authorized single nonroot host owner must exactly match.
Root-owned/mixed-owner state is still refused. No chown, chmod, supplemental group,
privileged initializer, root workload, capability, socket or host credential is
added. UID selection is not permission to alter ownership. Default image-user
policy for legacy stateless routes without this fact remains separate; curation
still requires actual nonroot kernel evidence before publication.

This does not solve Memos10001 or Mealie911 storage initialization on a host whose
state directory belongs to1000, provide UID remapping, or deploy a Runner. Their
mismatch must remain a refusal until the existing storage/Runner boundary can
safely provide the requested ownership. Numeric user selection does not reserve
capacity, approve source/build execution, grant network access or satisfy K.
