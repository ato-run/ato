// Keep the binary entrypoint intentionally thin so tests and alternate entry
// surfaces can reuse the same startup path from the library crate.
#[cfg(windows)]
fn main() {
    std::thread::Builder::new()
        .name("ato-main".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(cli::main_entry)
        .expect("failed to spawn ato main thread")
        .join()
        .expect("ato main thread panicked");
}

#[cfg(not(windows))]
fn main() {
    cli::main_entry();
}
