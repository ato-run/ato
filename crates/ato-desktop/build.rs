use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set by Cargo"),
    );

    // ato-desktop frontend (GPUI chrome UI panels)
    check_frontend_dist(&manifest_dir);

    // ato-onboarding system capsule (Vite + React)
    check_onboarding_dist(&manifest_dir);

    // ato-dock system capsule (Vite + React)
    check_dock_dist(&manifest_dir);

    // ato-start system capsule (Astro)
    check_start_dist(&manifest_dir);

    // ato-store system capsule (Astro desktop static build)
    check_store_dist(&manifest_dir);
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

fn check_dock_dist(manifest_dir: &PathBuf) {
    let dock_dir = manifest_dir.join("assets").join("system").join("ato-dock");
    let dist_dir = dock_dir.join("dist");

    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("App.jsx").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("index.html").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("capsule.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("package.json").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("package-lock.json").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("vite.config.js").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("src").join("main.jsx").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("src").join("index.css").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        dock_dir.join("src").join("bridge.js").display()
    );
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_DOCK_BUILD");

    let skip_requested = env_truthy("ATO_DESKTOP_SKIP_DOCK_BUILD");
    if skip_requested {
        if dist_dir.exists() {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_DOCK_BUILD=1 set; using existing dock dist at {}",
                dist_dir.display()
            );
            return;
        }

        panic!(
            "ATO_DESKTOP_SKIP_DOCK_BUILD=1 was set but dock dist is missing at {}. Unset the variable or build dist first.",
            dist_dir.display()
        );
    }

    if !dock_dir.join("node_modules").exists() {
        run_command(
            "npm",
            &["install"],
            &dock_dir,
            "ato-dock dependency install",
        );
    }

    run_command("npm", &["run", "build"], &dock_dir, "ato-dock build");

    if dist_dir.exists() {
        return;
    }

    panic!(
        "ato-dock dist missing at {} after build. Run `npm install && npm run build` in assets/system/ato-dock/ first.",
        dist_dir.display()
    );
}

fn run_command(binary: &str, args: &[&str], cwd: &PathBuf, label: &str) {
    let status = Command::new(binary).args(args).current_dir(cwd).status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            panic!(
                "{} failed with status {} in {}",
                label,
                status,
                cwd.display()
            );
        }
        Err(error) => {
            panic!(
                "failed to execute `{}` for {} in {}: {}",
                binary,
                label,
                cwd.display(),
                error
            );
        }
    }
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

fn check_store_dist(manifest_dir: &PathBuf) {
    let dist_dir = manifest_dir
        .join("assets")
        .join("system")
        .join("ato-store")
        .join("dist");

    println!("cargo:rerun-if-changed={}", dist_dir.display());
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_STORE_BUILD");

    let skip_requested = env_truthy("ATO_DESKTOP_SKIP_STORE_BUILD");
    if dist_dir.exists() {
        if skip_requested {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_STORE_BUILD=1 set; using existing store dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    panic!(
        "ato-store dist missing at {}. Run `cargo run --manifest-path xtask/Cargo.toml -- store build` first.",
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
