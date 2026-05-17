use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set by Cargo"),
    );

    // ato-desktop frontend (GPUI chrome UI panels)
    check_frontend_dist(&manifest_dir);

    // ato-onboarding system capsule (Vite + React)
    check_onboarding_dist(&manifest_dir);

    // ato-start system capsule (Astro)
    check_start_dist(&manifest_dir);
}

fn check_frontend_dist(manifest_dir: &PathBuf) {
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

fn check_onboarding_dist(manifest_dir: &PathBuf) {
    let dist_dir = manifest_dir
        .join("assets")
        .join("system")
        .join("ato-onboarding")
        .join("dist");

    println!("cargo:rerun-if-changed={}", dist_dir.display());
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_ONBOARDING_BUILD");

    let skip_requested = env_truthy("ATO_DESKTOP_SKIP_ONBOARDING_BUILD");
    if dist_dir.exists() {
        if skip_requested {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_ONBOARDING_BUILD=1 set; using existing onboarding dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    panic!(
        "ato-onboarding dist missing at {}. Run `npm install && npm run build` in assets/system/ato-onboarding/ first.",
        dist_dir.display()
    );
}

fn check_start_dist(manifest_dir: &PathBuf) {
    let dist_dir = manifest_dir
        .join("assets")
        .join("system")
        .join("ato-start")
        .join("dist");

    println!("cargo:rerun-if-changed={}", dist_dir.display());
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_START_BUILD");

    let skip_requested = env_truthy("ATO_DESKTOP_SKIP_START_BUILD");
    if dist_dir.exists() {
        if skip_requested {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_START_BUILD=1 set; using existing start dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    panic!(
        "ato-start dist missing at {}. Run `npm install && npm run build` in assets/system/ato-start/ first.",
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
