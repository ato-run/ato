#!/bin/sh

set -eu

APP="ato"
NACELLE_NAME="nacelle"
# Monorepo as the canonical release source. ato-run/ato-cli (legacy
# CLI repo) and Koh0920/capsuled (legacy nacelle repo) are archived
# from v0.4.88 onward; everything ships from ato-run/ato now.
ATO_RELEASE_REPO="${ATO_RELEASE_REPO:-ato-run/ato}"
ATO_DESKTOP_RELEASE_REPO="${ATO_DESKTOP_RELEASE_REPO:-${ATO_RELEASE_REPO}}"
NACELLE_RELEASE_REPO="${NACELLE_RELEASE_REPO:-ato-run/ato}"
ATO_GITHUB_API_BASE_URL="${ATO_GITHUB_API_BASE_URL:-https://api.github.com}"
ATO_GITHUB_RELEASE_BASE_URL="${ATO_GITHUB_RELEASE_BASE_URL:-https://github.com}"
ATO_INSTALL_DIR="${ATO_INSTALL_DIR:-$HOME/.local/bin}"
ATO_DESKTOP_INSTALL_DIR="${ATO_DESKTOP_INSTALL_DIR:-}"
ATO_SKIP_CARGO_FALLBACK="${ATO_SKIP_CARGO_FALLBACK:-0}"
ATO_SKIP_NACELLE_INSTALL="${ATO_SKIP_NACELLE_INSTALL:-0}"

requested_version="${ATO_RELEASE_VERSION:-${VERSION:-latest}}"
requested_desktop_version="${ATO_DESKTOP_RELEASE_VERSION:-latest}"
requested_nacelle_version="${NACELLE_RELEASE_VERSION:-latest}"
binary_path=""
no_modify_path=0
run_uninstall=0
# Plan B (v0.5): default route is `auto` — install Desktop bundle on
# graphical sessions, CLI-only on headless. `--cli-only` forces the
# minimal flow (no bundle, no quarantine strip) and is the right
# choice for CI runners and SSH sessions.
desktop_mode="${ATO_DESKTOP_MODE:-auto}"

MUTED='\033[0;2m'
RED='\033[0;31m'
ORANGE='\033[38;5;214m'
NC='\033[0m'

usage() {
    cat <<EOF
Ato Installer

Usage: install.sh [options]

Options:
    -h, --help              Display this help message
    -v, --version <version> Install a specific ato-cli version
    -b, --binary <path>     Install ato from a local binary instead of downloading
        --uninstall         Remove a previously-installed ato (delegates to scripts/uninstall.sh)
        --cli-only          Install only the CLI (skip Desktop bundle even on graphical sessions)
        --with-desktop      Force Desktop bundle install even when no display is detected
        --no-modify-path    Don't modify shell config files (.zshrc, .bashrc, etc.)

Environment:
    ATO_RELEASE_REPO          GitHub repo for ato-cli releases (default: ${ATO_RELEASE_REPO})
    ATO_DESKTOP_RELEASE_REPO  GitHub repo for ato-desktop releases (default: same as ATO_RELEASE_REPO)
    NACELLE_RELEASE_REPO      GitHub repo for nacelle releases (default: ${NACELLE_RELEASE_REPO})
    ATO_RELEASE_VERSION       ato-cli version to install (default: latest)
    ATO_DESKTOP_RELEASE_VERSION  Desktop bundle version (default: latest)
    NACELLE_RELEASE_VERSION   nacelle version to install (default: latest)
    ATO_INSTALL_DIR           CLI install directory (default: ${ATO_INSTALL_DIR})
    ATO_DESKTOP_INSTALL_DIR   Desktop bundle target dir (default: ~/Applications on macOS, ~/Applications on Linux)
    ATO_DESKTOP_MODE          auto | cli-only | desktop (default: auto)
    ATO_SKIP_NACELLE_INSTALL  Set to 1 to skip nacelle installation
    ATO_SKIP_CARGO_FALLBACK   Set to 1 to disable cargo fallback for ato-cli

Examples:
    curl -fsSL https://ato.run/install.sh | sh
    curl -fsSL https://ato.run/install.sh | sh -s -- --version 0.4.39
    ./install.sh --binary /path/to/ato
EOF
}

print_message() {
    color="${NC}"

    case "$1" in
        warning) color="${ORANGE}" ;;
        error) color="${RED}" ;;
    esac

    printf '%b%b%b\n' "${color}" "$2" "${NC}" >&2
}

fail() {
    print_message error "$1"
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "Required command not found: $1"
}

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        -v|--version)
            if [ -n "${2:-}" ]; then
                requested_version="$2"
                shift 2
            else
                fail "--version requires a version argument"
            fi
            ;;
        -b|--binary)
            if [ -n "${2:-}" ]; then
                binary_path="$2"
                shift 2
            else
                fail "--binary requires a path argument"
            fi
            ;;
        --no-modify-path)
            no_modify_path=1
            shift
            ;;
        --cli-only)
            desktop_mode="cli-only"
            shift
            ;;
        --with-desktop)
            desktop_mode="desktop"
            shift
            ;;
        --uninstall)
            run_uninstall=1
            shift
            ;;
        *)
            print_message warning "Warning: Unknown option '$1'"
            shift
            ;;
    esac
done

need_cmd uname
need_cmd mktemp
need_cmd curl

TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t ato-install)"
trap 'rm -rf "$TMP_DIR"' 0 1 2 15

normalize_release_tag() {
    case "$1" in
        latest) printf 'latest' ;;
        v*) printf '%s' "$1" ;;
        *) printf 'v%s' "$1" ;;
    esac
}

resolve_release_metadata() {
    rrm_repo="$1"
    rrm_version="$2"
    rrm_metadata_path="$3"
    rrm_release_endpoint=""

    if [ "$rrm_version" = "latest" ]; then
        rrm_release_endpoint="${ATO_GITHUB_API_BASE_URL%/}/repos/${rrm_repo}/releases/latest"
    else
        rrm_release_tag="$(normalize_release_tag "$rrm_version")"
        rrm_release_endpoint="${ATO_GITHUB_API_BASE_URL%/}/repos/${rrm_repo}/releases/tags/${rrm_release_tag}"
    fi

    curl -fsSL \
        --retry 2 \
        --connect-timeout 10 \
        --max-time 60 \
        -H "Accept: application/vnd.github+json" \
        -H "X-GitHub-Api-Version: 2022-11-28" \
        "$rrm_release_endpoint" \
        -o "$rrm_metadata_path"

    awk -F '"' '/"tag_name"[[:space:]]*:/ { print $4; exit }' "$rrm_metadata_path"
}

detect_target() {
    raw_os="$(uname -s)"
    os="$(echo "$raw_os" | tr '[:upper:]' '[:lower:]')"
    case "$raw_os" in
      Darwin*) os="darwin" ;;
      Linux*) os="linux" ;;
      MINGW*|MSYS*|CYGWIN*) os="windows" ;;
    esac

    arch="$(uname -m)"
        if [ "$arch" = "aarch64" ]; then
      arch="arm64"
    fi
        if [ "$arch" = "x86_64" ]; then
      arch="x64"
    fi

        if [ "$os" = "darwin" ] && [ "$arch" = "x64" ]; then
      rosetta_flag="$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)"
            if [ "$rosetta_flag" = "1" ]; then
        arch="arm64"
      fi
    fi

    case "$os-$arch" in
      linux-x64)
        ATO_TARGET="x86_64-unknown-linux-gnu"
        NACELLE_TARGET="linux-x64"
        ;;
      linux-arm64)
        ATO_TARGET="aarch64-unknown-linux-gnu"
        NACELLE_TARGET="linux-arm64"
        ;;
      darwin-x64)
        ATO_TARGET="x86_64-apple-darwin"
        NACELLE_TARGET="darwin-x64"
        ;;
      darwin-arm64)
        ATO_TARGET="aarch64-apple-darwin"
        NACELLE_TARGET="darwin-arm64"
        ;;
      *)
        fail "Unsupported OS/Arch: $os/$arch"
        ;;
    esac
    DETECTED_OS="$os"
}

download_file() {
    curl -fL --retry 2 --connect-timeout 10 --max-time 120 "$1" -o "$2"
}

install_into_dir() {
    iid_source_path="$1"
    iid_target_name="$2"

    # Replace via a sibling temp file so Linux can update a running executable.
    mkdir -p "$ATO_INSTALL_DIR" || return 1
    iid_temp_path="$(mktemp "${ATO_INSTALL_DIR}/.${iid_target_name}.tmp.XXXXXX")" || return 1

    cp "$iid_source_path" "$iid_temp_path" || {
        rm -f "$iid_temp_path"
        return 1
    }
    chmod 0755 "$iid_temp_path" || {
        rm -f "$iid_temp_path"
        return 1
    }
    mv -f "$iid_temp_path" "$ATO_INSTALL_DIR/$iid_target_name" || {
        rm -f "$iid_temp_path"
        return 1
    }
}

download_ato_archive() {
    daa_tag="$1"
    daa_version="$2"
    daa_archive_path="$3"
    daa_release_base="${ATO_GITHUB_RELEASE_BASE_URL%/}/${ATO_RELEASE_REPO}/releases/download/${daa_tag}"

    for archive_name in \
        "ato-cli-${daa_version}-${ATO_TARGET}.tar.xz" \
        "ato-cli-${ATO_TARGET}.tar.xz" \
        "ato-cli-${daa_version}-${ATO_TARGET}.tar.gz" \
        "ato-cli-${ATO_TARGET}.tar.gz"
    do
        if download_file "${daa_release_base}/${archive_name}" "$daa_archive_path"; then
            printf '%s' "$archive_name"
            return 0
        fi
    done

    return 1
}

install_ato_from_archive() {
    ifa_metadata_path="$TMP_DIR/ato-release.json"
    ifa_extract_dir="$TMP_DIR/ato-extract"
    ifa_archive_path="$TMP_DIR/ato-archive"

    need_cmd tar

    print_message info "${MUTED}Resolving ato-cli release from GitHub${NC}"
    ATO_RESOLVED_TAG="$(resolve_release_metadata "$ATO_RELEASE_REPO" "$requested_version" "$ifa_metadata_path")" || return 1
    [ -n "$ATO_RESOLVED_TAG" ] || fail "GitHub release metadata for ato-cli did not contain tag_name"
    ifa_resolved_version="${ATO_RESOLVED_TAG#v}"
    ifa_archive_name="$(download_ato_archive "$ATO_RESOLVED_TAG" "$ifa_resolved_version" "$ifa_archive_path")" || return 1

    mkdir -p "$ifa_extract_dir"
    case "$ifa_archive_name" in
        *.tar.xz) tar -xJf "$ifa_archive_path" -C "$ifa_extract_dir" ;;
        *.tar.gz) tar -xzf "$ifa_archive_path" -C "$ifa_extract_dir" ;;
        *) return 1 ;;
    esac

    ifa_binary_candidate="$ifa_extract_dir/$APP"
    ifa_archive_dir="${ifa_archive_name%.tar.xz}"
    ifa_archive_dir="${ifa_archive_dir%.tar.gz}"
    if [ ! -f "$ifa_binary_candidate" ] && [ -f "$ifa_extract_dir/$ifa_archive_dir/$APP" ]; then
        ifa_binary_candidate="$ifa_extract_dir/$ifa_archive_dir/$APP"
    fi
    [ -f "$ifa_binary_candidate" ] || return 1

    install_into_dir "$ifa_binary_candidate" "$APP" || return 1
}

install_ato_from_binary() {
    [ -f "$binary_path" ] || fail "Binary not found at ${binary_path}"
    install_into_dir "$binary_path" "$APP" || fail "Failed to install ato to ${ATO_INSTALL_DIR}"
    ATO_RESOLVED_TAG="local"
}

install_ato_via_cargo() {
    need_cmd cargo
    iac_cargo_root="$TMP_DIR/cargo-root"
    mkdir -p "$iac_cargo_root"

    print_message info "${MUTED}Falling back to cargo install for ato-cli${NC}"
    iac_tag_value=""
    if [ "$requested_version" != "latest" ]; then
        iac_tag_value="$(normalize_release_tag "$requested_version")"
    elif [ -n "${ATO_RESOLVED_TAG:-}" ] && [ "$ATO_RESOLVED_TAG" != "local" ]; then
        iac_tag_value="$ATO_RESOLVED_TAG"
    fi

    if [ -n "$iac_tag_value" ]; then
        cargo install \
            --git "https://github.com/${ATO_RELEASE_REPO}.git" \
            ato-cli \
            --bin ato \
            --locked \
            --force \
            --root "$iac_cargo_root" \
            --tag "$iac_tag_value"
    else
        cargo install \
            --git "https://github.com/${ATO_RELEASE_REPO}.git" \
            ato-cli \
            --bin ato \
            --locked \
            --force \
            --root "$iac_cargo_root"
    fi

    [ -f "$iac_cargo_root/bin/ato" ] || fail "cargo install finished, but ato binary was not found"
    install_into_dir "$iac_cargo_root/bin/ato" "$APP" || fail "Failed to install ato to ${ATO_INSTALL_DIR}"
}

install_nacelle() {
    in_metadata_path="$TMP_DIR/nacelle-release.json"
    in_nacelle_tmp="$TMP_DIR/$NACELLE_NAME"

    print_message info "${MUTED}Resolving nacelle release from GitHub${NC}"
    in_release_tag="$(resolve_release_metadata "$NACELLE_RELEASE_REPO" "$requested_nacelle_version" "$in_metadata_path")" || return 1
    [ -n "$in_release_tag" ] || fail "GitHub release metadata for nacelle did not contain tag_name"
    in_release_base="${ATO_GITHUB_RELEASE_BASE_URL%/}/${NACELLE_RELEASE_REPO}/releases/download/${in_release_tag}"

    # nacelle's release artifacts come from cargo-dist now. Asset
    # names use the rust target triple wrapped in a tarball, e.g.
    #   nacelle-aarch64-apple-darwin.tar.xz
    # Older legacy assets (`nacelle-vX.Y.Z-darwin-arm64` raw binary
    # from Koh0920/capsuled) remain as a fallback so users overriding
    # NACELLE_RELEASE_REPO to the legacy repo still work.
    in_archive_path="$TMP_DIR/${NACELLE_NAME}.tar.xz"
    in_extract_dir="$TMP_DIR/nacelle-extract"

    if download_file "${in_release_base}/${NACELLE_NAME}-${ATO_TARGET}.tar.xz" "$in_archive_path"; then
        need_cmd tar
        rm -rf "$in_extract_dir"
        mkdir -p "$in_extract_dir"
        tar -xJf "$in_archive_path" -C "$in_extract_dir" || return 1
        in_nacelle_candidate="$in_extract_dir/$NACELLE_NAME"
        in_archive_inner_dir="${NACELLE_NAME}-${ATO_TARGET}"
        if [ ! -f "$in_nacelle_candidate" ] && [ -f "$in_extract_dir/$in_archive_inner_dir/$NACELLE_NAME" ]; then
            in_nacelle_candidate="$in_extract_dir/$in_archive_inner_dir/$NACELLE_NAME"
        fi
        [ -f "$in_nacelle_candidate" ] || return 1
        install_into_dir "$in_nacelle_candidate" "$NACELLE_NAME" || return 1
        return 0
    fi

    # Legacy (raw-binary) fallback for users still pointing at
    # Koh0920/capsuled. Drop this branch once that repo is archived.
    case "$DETECTED_OS-$NACELLE_TARGET" in
        darwin-darwin-arm64)
            set -- \
                "${NACELLE_NAME}-${in_release_tag}-darwin-arm64" \
                "${NACELLE_NAME}-${in_release_tag}-macos-arm64" \
                "${NACELLE_NAME}-${in_release_tag}-macos-universal"
            ;;
        darwin-darwin-x64)
            set -- \
                "${NACELLE_NAME}-${in_release_tag}-darwin-x64" \
                "${NACELLE_NAME}-${in_release_tag}-macos-x64" \
                "${NACELLE_NAME}-${in_release_tag}-macos-universal"
            ;;
        linux-linux-arm64)
            set -- \
                "${NACELLE_NAME}-${in_release_tag}-linux-arm64" \
                "${NACELLE_NAME}-${in_release_tag}-linux-aarch64"
            ;;
        linux-linux-x64)
            set -- \
                "${NACELLE_NAME}-${in_release_tag}-linux-x64" \
                "${NACELLE_NAME}-${in_release_tag}-linux-x86_64"
            ;;
        *)
            set -- "${NACELLE_NAME}-${in_release_tag}-${NACELLE_TARGET}"
            ;;
    esac

    for asset_name in "$@"; do
        if download_file "${in_release_base}/${asset_name}" "$in_nacelle_tmp"; then
            install_into_dir "$in_nacelle_tmp" "$NACELLE_NAME" || return 1
            return 0
        fi
    done

    return 1
}

add_to_path() {
    atp_config_file=$1
    atp_command=$2

    if grep -Fxq "$atp_command" "$atp_config_file"; then
        print_message info "Command already exists in $atp_config_file, skipping write."
    elif [ -w "$atp_config_file" ]; then
        printf '\n# ato\n' >> "$atp_config_file"
        printf '%s\n' "$atp_command" >> "$atp_config_file"
        print_message info "${MUTED}Successfully added ${NC}${ATO_INSTALL_DIR} ${MUTED}to PATH in ${NC}$atp_config_file"
    else
        print_message warning "Manually add the directory to $atp_config_file (or similar):"
        print_message info "  $atp_command"
    fi
}

update_path_if_needed() {
    up_xdg_config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
    up_current_shell="${SHELL##*/}"
    up_config_files=""
    up_config_file=""

    if [ -z "$up_current_shell" ]; then
        up_current_shell="sh"
    fi

    case $up_current_shell in
        fish)
            up_config_files="$HOME/.config/fish/config.fish"
            ;;
        zsh)
            up_config_files="${ZDOTDIR:-$HOME}/.zshrc ${ZDOTDIR:-$HOME}/.zshenv $up_xdg_config_home/zsh/.zshrc $up_xdg_config_home/zsh/.zshenv"
            ;;
        bash)
            up_config_files="$HOME/.bashrc $HOME/.bash_profile $HOME/.profile $up_xdg_config_home/bash/.bashrc $up_xdg_config_home/bash/.bash_profile"
            ;;
        *)
            up_config_files="$HOME/.profile $HOME/.bashrc"
            ;;
    esac

    for file in $up_config_files; do
        if [ -f "$file" ]; then
            up_config_file=$file
            break
        fi
    done

    case ":$PATH:" in
        *":$ATO_INSTALL_DIR:"*)
            return 0
            ;;
    esac

    if [ -z "$up_config_file" ]; then
        print_message warning "No config file found for $up_current_shell. Add this manually:"
        print_message info "  export PATH=$ATO_INSTALL_DIR:\$PATH"
        return 0
    fi

    case $up_current_shell in
        fish)
            add_to_path "$up_config_file" "fish_add_path $ATO_INSTALL_DIR"
            ;;
        *)
            add_to_path "$up_config_file" "export PATH=$ATO_INSTALL_DIR:\$PATH"
            ;;
    esac
}

# Headless detection — pure heuristics, no GUI probe.
#
# Multi-signal so we don't false-positive on Wayland-only desktops or
# remote sessions where DISPLAY happens to be inherited:
#   - macOS: `launchctl print gui/$(id -u)` exits 0 only inside an
#     active GUI Aqua session. SSH sessions get launchd's user agent
#     scope but no gui/ scope, so this is the cleanest signal.
#   - Linux: any of DISPLAY / WAYLAND_DISPLAY / XDG_SESSION_TYPE=(x11|wayland)
#     indicates a graphical session.
is_headless_session() {
    case "$DETECTED_OS" in
        darwin)
            if launchctl print "gui/$(id -u)" >/dev/null 2>&1; then
                return 1  # graphical session
            fi
            return 0  # headless / SSH
            ;;
        linux)
            if [ -n "${DISPLAY:-}" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; then
                return 1
            fi
            case "${XDG_SESSION_TYPE:-}" in
                x11|wayland) return 1 ;;
            esac
            return 0
            ;;
        *)
            # Windows shouldn't take this code path (install-win.ps1
            # handles it) but if a user runs install.sh under WSL we
            # treat it as headless — Desktop builds aren't WSL-aware.
            return 0
            ;;
    esac
}

# Resolve the effective desktop mode after argv + env + autodetection.
# After this runs, `desktop_mode` is one of: "desktop" | "cli-only".
resolve_desktop_mode() {
    case "$desktop_mode" in
        desktop|cli-only)
            return 0  # explicit override, respect it
            ;;
        auto)
            case "$DETECTED_OS" in
                darwin|linux)
                    if is_headless_session; then
                        desktop_mode="cli-only"
                        print_message info "${MUTED}No display detected — installing CLI only. Pass --with-desktop to override.${NC}"
                    else
                        desktop_mode="desktop"
                    fi
                    ;;
                *)
                    desktop_mode="cli-only"
                    ;;
            esac
            ;;
        *)
            print_message warning "Unknown ATO_DESKTOP_MODE='${desktop_mode}', falling back to cli-only."
            desktop_mode="cli-only"
            ;;
    esac
}

# Default Desktop install dir — user-scoped to keep `curl | sh` from
# needing sudo. /Applications is the macOS convention but writes
# require admin; ~/Applications is searched by Spotlight + Finder.
default_desktop_install_dir() {
    case "$DETECTED_OS" in
        darwin) printf '%s' "${HOME}/Applications" ;;
        linux)  printf '%s' "${HOME}/Applications" ;;
        *)      printf '%s' "${HOME}/Applications" ;;
    esac
}

download_desktop_zip() {
    ddz_tag="$1"
    ddz_version="$2"
    ddz_dest="$3"
    ddz_arch_suffix="$4"
    ddz_release_base="${ATO_GITHUB_RELEASE_BASE_URL%/}/${ATO_DESKTOP_RELEASE_REPO}/releases/download/${ddz_tag}"
    download_file \
        "${ddz_release_base}/Ato-Desktop-${ddz_version}-${ddz_arch_suffix}.zip" \
        "$ddz_dest"
}

download_desktop_appimage() {
    dda_tag="$1"
    dda_version="$2"
    dda_dest="$3"
    dda_release_base="${ATO_GITHUB_RELEASE_BASE_URL%/}/${ATO_DESKTOP_RELEASE_REPO}/releases/download/${dda_tag}"
    dda_arch=""
    case "$ATO_TARGET" in
        x86_64-unknown-linux-gnu)  dda_arch="x86_64" ;;
        aarch64-unknown-linux-gnu) dda_arch="aarch64" ;;
        *) return 1 ;;
    esac
    download_file \
        "${dda_release_base}/Ato-Desktop-${dda_version}-${dda_arch}.AppImage" \
        "$dda_dest"
}

install_desktop_macos() {
    idm_metadata_path="$TMP_DIR/desktop-release.json"
    idm_zip_path="$TMP_DIR/Ato-Desktop.zip"
    idm_extract_dir="$TMP_DIR/Ato-Desktop-extract"
    idm_target_dir="${ATO_DESKTOP_INSTALL_DIR:-$(default_desktop_install_dir)}"
    idm_arch_suffix=""
    case "$ATO_TARGET" in
        aarch64-apple-darwin) idm_arch_suffix="darwin-arm64" ;;
        x86_64-apple-darwin)  idm_arch_suffix="darwin-x86_64" ;;
        *) return 1 ;;
    esac

    need_cmd unzip

    print_message info "${MUTED}Resolving ato-desktop release${NC}"
    idm_tag="$(resolve_release_metadata "$ATO_DESKTOP_RELEASE_REPO" "$requested_desktop_version" "$idm_metadata_path")" || return 1
    [ -n "$idm_tag" ] || return 1
    idm_version="${idm_tag#ato-desktop-v}"
    idm_version="${idm_version#v}"

    download_desktop_zip "$idm_tag" "$idm_version" "$idm_zip_path" "$idm_arch_suffix" || return 1

    mkdir -p "$idm_target_dir"
    rm -rf "$idm_extract_dir"
    mkdir -p "$idm_extract_dir"
    # The zip is built by `ditto -c -k --keepParent` so the top entry
    # is `Ato Desktop.app/`. unzip preserves the codesign xattrs that
    # tar -xz would have stripped.
    unzip -q "$idm_zip_path" -d "$idm_extract_dir" || return 1
    [ -d "$idm_extract_dir/Ato Desktop.app" ] || return 1

    # Atomic-ish replace of any prior install. /Applications writes
    # need admin so the default target is ~/Applications; users who
    # want /Applications should `sudo` and pass ATO_DESKTOP_INSTALL_DIR.
    rm -rf "$idm_target_dir/Ato Desktop.app" 2>/dev/null || true
    mv "$idm_extract_dir/Ato Desktop.app" "$idm_target_dir/" || return 1

    # Curl + unzip never sets com.apple.quarantine, so we don't need
    # the xattr -dr workaround the .dmg path required. Verify just in
    # case the user piped the zip through a quarantine-tagging path.
    xattr -dr com.apple.quarantine "$idm_target_dir/Ato Desktop.app" 2>/dev/null || true

    DESKTOP_INSTALLED_PATH="$idm_target_dir/Ato Desktop.app"
}

install_desktop_linux() {
    idl_metadata_path="$TMP_DIR/desktop-release.json"
    idl_target_dir="${ATO_DESKTOP_INSTALL_DIR:-$(default_desktop_install_dir)}"
    mkdir -p "$idl_target_dir"

    print_message info "${MUTED}Resolving ato-desktop release${NC}"
    idl_tag="$(resolve_release_metadata "$ATO_DESKTOP_RELEASE_REPO" "$requested_desktop_version" "$idl_metadata_path")" || return 1
    [ -n "$idl_tag" ] || return 1
    idl_version="${idl_tag#ato-desktop-v}"
    idl_version="${idl_version#v}"

    idl_appimage_path="$idl_target_dir/Ato-Desktop.AppImage"
    download_desktop_appimage "$idl_tag" "$idl_version" "$idl_appimage_path" || return 1
    chmod 0755 "$idl_appimage_path" || return 1
    DESKTOP_INSTALLED_PATH="$idl_appimage_path"
}

install_desktop_bundle() {
    case "$DETECTED_OS" in
        darwin) install_desktop_macos ;;
        linux)  install_desktop_linux ;;
        *)      return 1 ;;
    esac
}

print_post_install() {
    printf '\n'
    print_message info "${MUTED}Installed:${NC} $ATO_INSTALL_DIR/$APP"
    if [ "$ATO_SKIP_NACELLE_INSTALL" != "1" ] && [ -x "$ATO_INSTALL_DIR/$NACELLE_NAME" ]; then
        print_message info "${MUTED}Installed:${NC} $ATO_INSTALL_DIR/$NACELLE_NAME"
    fi
    if [ -n "${DESKTOP_INSTALLED_PATH:-}" ]; then
        print_message info "${MUTED}Installed Desktop:${NC} $DESKTOP_INSTALLED_PATH"
    fi
    case ":$PATH:" in
        *":$ATO_INSTALL_DIR:"*)
        print_message info "${MUTED}Try:${NC} ato --version"
            ;;
        *)
        print_message info "${MUTED}Add to PATH:${NC} export PATH=$ATO_INSTALL_DIR:\$PATH"
            ;;
    esac
}

detect_target

# --uninstall short-circuits the install path. We delegate to the
# stand-alone uninstall.sh in the monorepo so the logic stays single
# source of truth (mirrors `ato uninstall` for users without a
# functioning CLI).
if [ "$run_uninstall" = "1" ]; then
    uninstall_script_url="${ATO_UNINSTALL_SCRIPT_URL:-https://raw.githubusercontent.com/${ATO_RELEASE_REPO}/main/scripts/uninstall.sh}"
    print_message info "${MUTED}Fetching uninstall script: ${NC}${uninstall_script_url}"
    uninstall_path="$TMP_DIR/ato-uninstall.sh"
    download_file "$uninstall_script_url" "$uninstall_path" || fail "Failed to download uninstall script from ${uninstall_script_url}"
    sh "$uninstall_path"
    exit $?
fi

resolve_desktop_mode
mkdir -p "$ATO_INSTALL_DIR"

if [ -n "$binary_path" ]; then
    install_ato_from_binary
else
    if ! install_ato_from_archive; then
        print_message warning "ato-cli binary download failed for $ATO_TARGET"
        if [ "$ATO_SKIP_CARGO_FALLBACK" = "1" ]; then
            fail "Set ATO_SKIP_CARGO_FALLBACK=0 (or unset it) to allow cargo fallback."
        fi
        install_ato_via_cargo
    fi
fi

if [ "$ATO_SKIP_NACELLE_INSTALL" = "1" ]; then
    print_message info "${MUTED}Skipping nacelle install (ATO_SKIP_NACELLE_INSTALL=1)${NC}"
else
    if ! install_nacelle; then
        fail "Failed to install nacelle from GitHub Releases (${NACELLE_RELEASE_REPO})."
    fi
fi

# Desktop bundle is best-effort: a failure to download / unzip the
# bundle should NOT abort the CLI install we already finished. The
# Homebrew Cask was retired in v0.4.88 (it shipped a quarantined .dmg),
# so the manual fallback now points at the GitHub Release zip.
if [ "$desktop_mode" = "desktop" ]; then
    if ! install_desktop_bundle; then
        print_message warning "Desktop bundle install failed — continuing with CLI-only."
        print_message info "${MUTED}Download manually:${NC} https://github.com/${ATO_DESKTOP_RELEASE_REPO}/releases"
    fi
fi

if [ "$no_modify_path" != "1" ]; then
    update_path_if_needed
fi

if [ -n "${GITHUB_ACTIONS-}" ] && [ "${GITHUB_ACTIONS}" = "true" ]; then
    echo "$ATO_INSTALL_DIR" >> "$GITHUB_PATH"
fi

print_post_install
