# Homebrew tap deployment

The `Casks/ato.rb` template in this directory is the source of truth for
the Homebrew Cask that ships **Ato Desktop + bundled CLI helper**. The
file is mirrored to the external [`ato-run/homebrew-ato`](https://github.com/ato-run/homebrew-ato)
tap repository on each Desktop release.

## Why this lives here

Per CPDS §4.2.2 the Cask is a release artifact, not a project of its
own. Co-locating the template with the bundle that produces the .dmg
keeps review honest:

- Bundle layout changes (e.g. the path to `Helpers/ato`) must update
  this file in lock-step.
- Versioning and SHA injection are mechanical post-release tasks — the
  template never carries a real checksum, only `sha256 :no_check`.
- The auto-generated `Formula/ato.rb` from cargo-dist (CLI-only) lives
  alongside this Cask in the tap repo. That file is generated; this
  one is hand-edited.

## Sync workflow

Triggered manually after `desktop-release` publishes a new tag:

```sh
# 1. Compute the SHA256 of each .dmg in the GitHub Release
# 2. Copy this Casks/ato.rb into ato-run/homebrew-ato
# 3. Replace `sha256 :no_check` with the per-arch checksums
# 4. Bump `version "x.y.z"` to match the released tag
# 5. brew audit --strict --cask ato
# 6. PR into ato-run/homebrew-ato
```

A scripted version will land alongside PR-12 (release automation) per
the v0.5 plan §7-6.

## Why ad-hoc + `auto_updates true`

The v0.5 distribution plan (D-3) chose ad-hoc codesigning over the
Apple Developer Program. `auto_updates true` is the documented
workaround that lets Homebrew install ad-hoc-signed Casks without
hitting Gatekeeper friction every launch — see the comment block in
`Casks/ato.rb` for the full rationale.

When v0.6 procures a Developer ID, the `auto_updates` line can stay
(it accurately describes our update model — we do not ship Sparkle)
but the codesign mode flip will eliminate the original motivation for
including it.
