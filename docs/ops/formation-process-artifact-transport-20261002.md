# Process artifact transport — 2026-10-02

Trial 11 at Ato 5143a6dcbdad02c340a050e01906d16ae2a6ded9 / API
4af64035203e767088edf70bcbb5ab42c9c38b3b built changedetection.io's three
sdists (feedgen, jstyleson, websockets), hash-installed 168 completed wheels,
sealed, launched and obtained a fresh PASS for the original K. Publication
still failed: 573,931,008 transport bytes versus the existing 536,870,912-byte
ticket cap. The previous 831,707,648-byte failure is preserved. This trial's
result SHA-256 is 46e1b7c7aac9ae7dcb2dbe900fdbd7c314ef23b968c6ba24ba2c7e8c4d45c756;
attempt 01M3WW6EVTT1XFNW53WC80M95H, D
298cb3ac5d58809ba45eb09ef182cbb05ac5fbd1ff55ddeaafbcd8e3c6f153df.
K ba864ae817c507b0b277c2d657390ba25fb7f78ecc9566150fd0a7040be36fb6
remained fully satisfied, but no D was submitted. All reservations settled.

Keep the entire process workspace, including completed wheels, original
sdists and provenance, in a deterministic gzip transport when its tar exceeds
8 MiB and compression reduces its size. Small transport bytes stay unchanged.
Both local materialization storage and retained descriptor preparation use
the same packer. Transport content identity follows its exact compressed
bytes; source closure, logical K/D and the complete expanded tree are unchanged.
Existing common archive verification detects gzip and continues to enforce
the original file, containment and expanded-byte limits. No cap is raised.

Validation: deterministic compression, identical source closure, file-backed
verification and materialization (including an empty directory and contained
symlink), unchanged small archive bytes, and rejection under a smaller expanded
limit PASS. Existing pack tests (8) and retained replay tests (3) PASS; Worker
all-targets clippy and formatting PASS. Linux actual final-code publication
and separate functional acceptance remain required. Receipt authority code
is unchanged from 5143a6dc; API authority remains independently pinned there.
No deployment, remote migration, ordinary Run approval or 100-case measurement.
