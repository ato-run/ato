/// Maps a Rust target triple to the corresponding Bun release platform identifier.
///
/// Returns `None` for platforms where Bun does not publish pre-built binaries.
pub(crate) fn bun_platform_triple(rust_triple: &str) -> Option<&'static str> {
    match rust_triple {
        "aarch64-apple-darwin" => Some("darwin-aarch64"),
        "x86_64-apple-darwin" => Some("darwin-x86_64"),
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => Some("linux-x64"),
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => Some("linux-aarch64"),
        "x86_64-pc-windows-msvc" => Some("windows-x64.exe"),
        _ => None,
    }
}
