use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set by Cargo"),
    );
    let dist_dir = manifest_dir.join("frontend").join("dist");

    println!("cargo:rerun-if-changed={}", dist_dir.display());
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_FRONTEND_BUILD");

    let skip_requested = env_truthy("ATO_DESKTOP_SKIP_FRONTEND_BUILD");
    if dist_dir.exists() {
        if skip_requested {
            println!(
				"cargo:warning=ATO_DESKTOP_SKIP_FRONTEND_BUILD=1 set; using existing frontend dist at {}",
				dist_dir.display()
			);
        }
        return;
    }

    panic!(
		"frontend dist missing at {}. Run `cargo run --manifest-path xtask/Cargo.toml -- frontend build` first.",
		dist_dir.display()
	);
}

fn env_truthy(key: &str) -> bool {
    match env::var(key) {
        Ok(value) => {
            let trimmed = value.trim();
            !trimmed.is_empty() && !matches!(trimmed, "0" | "false" | "off" | "no")
        }
        Err(_) => false,
    }
}
