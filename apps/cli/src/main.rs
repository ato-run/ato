fn main() {
    if let Err(error) = ato_cli::run() {
        eprintln!("error: {error:#}");
        std::process::exit(ato_cli::cli_exit_code(&error));
    }
}
