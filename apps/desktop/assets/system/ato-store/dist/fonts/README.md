Please add a local Inter Bold font file at `public/fonts/Inter-Bold.ttf`.

Why: the OG Pages Function fetches `/fonts/Inter-Bold.ttf` from your origin for fast,
reliable font loading at the Edge. If the file is not present, the generator
falls back to a small bundled TTF (dev-only), but for production you should
include a real Inter-Bold.ttf to ensure consistent rendering.

Suggested file: Inter-Bold.ttf (from the Inter family). Place into this folder
and commit.
