use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum BindingCommands {
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        owner_scope: Option<String>,
        #[arg(long)]
        service_name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Inspect {
        binding_ref: String,
        #[arg(long)]
        json: bool,
    },
    Resolve {
        #[arg(long)]
        owner_scope: String,
        #[arg(long)]
        service_name: String,
        #[arg(long, default_value = "ingress")]
        binding_kind: String,
        #[arg(long)]
        caller_service: Option<String>,
        #[arg(long)]
        json: bool,
    },
    BootstrapTls {
        #[arg(long = "binding")]
        binding_ref: String,
        #[arg(long, default_value_t = false)]
        install_system_trust: bool,
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    ServeIngress {
        #[arg(long = "binding")]
        binding_ref: String,
        #[arg(long, default_value = ".")]
        manifest: PathBuf,
        #[arg(long)]
        upstream_url: Option<String>,
    },
    RegisterIngress {
        #[arg(long, default_value = ".")]
        manifest: PathBuf,
        #[arg(long)]
        service_name: String,
        #[arg(long)]
        url: String,
        #[arg(long)]
        json: bool,
    },
    RegisterService {
        #[arg(long, default_value = ".")]
        manifest: PathBuf,
        #[arg(long)]
        service_name: String,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        process_id: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        json: bool,
    },
    SyncProcess {
        #[arg(long)]
        process_id: String,
        #[arg(long)]
        json: bool,
    },
}
