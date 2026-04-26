// Keep the binary entrypoint intentionally thin so tests and alternate entry
// surfaces can reuse the same startup path from the library crate.
fn main() {
    ato_cli::main_entry();
}
