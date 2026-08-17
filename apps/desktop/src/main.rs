use anyhow::Result;
use ato_ipc::computation::ComputationCommand;

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("--version" | "-V") => println!("ato-desktop {}", env!("CARGO_PKG_VERSION")),
        Some("--run") => {
            let capsule_file = arguments
                .next()
                .ok_or_else(|| anyhow::anyhow!("--run requires a portable .capsule file"))?;
            let result = ato_desktop::dispatch(&ComputationCommand::RunPortable { capsule_file })?;
            print!("{}", result.output);
            if !result.success {
                std::process::exit(1);
            }
        }
        Some(other) => anyhow::bail!("unknown desktop argument: {other}"),
        None => ato_desktop::launch_console()?,
    }
    Ok(())
}
