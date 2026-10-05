# Expired Search reporting — 2026-10-02

The actual isolated owner-input Search
search_43b7abcf28ad5453e27ff9245a4a43ab used an explicitly preregistered
60-second fault-test deadline (defaults remain 30 minutes / 10 minutes).
Its matching reusable configuration was revoked, so it stopped at needs_input.
After expiry, owner registration returned HTTP 409 search_input_closed, with
created_at, deadline and attempt budget unchanged. Runtime settled inconclusive;
the real Coordinator reports unsatisfied, one attempt, no byte reservations.

At e02, resuming this Search with the original config/journal failed before
printing its result: coordinator request deadline elapsed; operation preserved.
The execution deadline was incorrectly also applied to reading settled results.
The CLI now uses a separate ordinary HTTP timeout for persisted Search/result
observations. New submissions, claims, decisions and inference remain on the
original bounded client. It reports settlement before considering new work,
never displays an expired input as resumable, and if only the requester wait
expires, labels that separately without inventing a durable settlement or
resolving UNKNOWN. No retry, round, budget or Search expiry is reset.

CLI clippy with warnings denied, formatting and Linux binary build pass. The
frozen bbd36877 CLI on the same preserved Search now prints the authenticated
deadline_exceeded result: one inference, one Runtime attempt, original deadline
22:15:48.952Z on October 1 UTC, no new inference or execution after expiry.
No byte reservations remain. Actual API/Runtime pins remain fe541c31/e02d4ca3
for this existing Search; this is a reporting regression, not new application
acceptance of the rebuilt authority. Result SHA-256:
cb3550ca2ec9b97bb5609be02d5be6d4803d944d98a367b4f1d7f29df940cb14.
No deployment, remote migration or ordinary Run permission.
