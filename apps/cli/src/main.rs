fn main() {
    if let Err(error) = ato_cli::run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
