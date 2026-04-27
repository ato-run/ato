# Homebrew Cask for Ato Desktop (Plan B, ad-hoc signed).
#
# This file is the source-of-truth template; it ships in the
# ato-desktop repo so review and template changes track the bundle.
# Deployment target is the `ato-run/homebrew-ato` tap (Casks/ato.rb)
# — sync via the helper script in installer/homebrew/sync.sh once
# release artifacts are published. Spec: CPDS §4.2.2.
#
# Why a Cask alongside the cargo-dist `Formula/ato.rb`:
# - The Formula installs the *CLI only* (Plan B `--cli-only` path).
# - This Cask installs the *Desktop bundle* and exposes the bundled
#   `ato` helper as a Cask `binary` so `brew install --cask ato` gets
#   both `ato-desktop` (GUI) and `ato` (CLI) wired into PATH.
#
# `auto_updates true` is intentional: it tells Homebrew that updates
# are managed outside Cask (we do not ship Sparkle), and it has the
# side-effect of letting Homebrew accept ad-hoc-signed bundles
# without manual `xattr -dr com.apple.quarantine` chasing. v0.6 will
# flip to Developer ID and may relax this.
cask "ato" do
  version "0.4.86"
  # NOTE: sha256 is replaced by the publish script after the .dmg is
  # uploaded. Keeping it as `:no_check` here so this template lints
  # cleanly outside of a real release; the synced tap copy MUST have
  # a real checksum (CPDS §5).
  sha256 :no_check

  # Apple Silicon and Intel share the same Cask but different DMGs.
  # Homebrew's `on_arm` / `on_intel` blocks resolve at install time.
  # Unified `v*` release tag: cargo-dist publishes the CLI artifacts
  # there and desktop-release.yml appends the Desktop bundles to the
  # same tag, so the Cask URL drops the legacy `ato-desktop-v*` prefix.
  on_arm do
    url "https://github.com/ato-run/ato/releases/download/v#{version}/Ato-Desktop-#{version}-darwin-arm64.dmg"
  end
  on_intel do
    url "https://github.com/ato-run/ato/releases/download/v#{version}/Ato-Desktop-#{version}-darwin-x86_64.dmg"
  end

  name "Ato Desktop"
  desc "Run sandboxed app capsules locally"
  homepage "https://ato.run"

  livecheck do
    url :url
    strategy :github_latest
  end

  # Beta-quality platforms still get the bundle; gate on Ventura
  # because xtask's bundle_macos_app sets LSMinimumSystemVersion to
  # 13.0 (matches Info.plist).
  depends_on macos: ">= :ventura"

  # `auto_updates true` must come before the install stanzas so
  # Homebrew classifies this Cask correctly during audit.
  auto_updates true

  app "Ato Desktop.app"

  # Expose the bundled CLI helper. This is the same binary that
  # cli_install.rs would symlink for direct-download users; routing
  # both install paths through the same Helpers/ato keeps version
  # skew impossible (CCP guarantee from PR-1).
  binary "#{appdir}/Ato Desktop.app/Contents/Helpers/ato"

  zap trash: [
    "~/Library/Application Support/Ato",
    "~/Library/Caches/run.ato.desktop",
    "~/Library/Logs/run.ato.desktop",
    "~/Library/Preferences/run.ato.desktop.plist",
    "~/.ato/desktop",
  ]
end
