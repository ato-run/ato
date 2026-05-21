use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set by Cargo"),
    );

    // ato-onboarding system capsule (Vite + React)
    check_onboarding_dist(&manifest_dir);

    // ato-dock system capsule (Vite + React)
    check_dock_dist(&manifest_dir);

    // ato-start system capsule (Astro)
    check_start_dist(&manifest_dir);

    // ato-store system capsule (Astro desktop static build)
    check_store_dist(&manifest_dir);
}

fn check_onboarding_dist(manifest_dir: &PathBuf) {
    let capsule_dir = manifest_dir
        .join("assets")
        .join("system")
        .join("ato-onboarding");
    let dist_dir = capsule_dir.join("dist");
    let entrypoint = dist_dir.join("index.html");

    println!("cargo:rerun-if-changed={}", dist_dir.display());
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_ONBOARDING_BUILD");

    if entrypoint.exists() {
        if env_truthy("ATO_DESKTOP_SKIP_ONBOARDING_BUILD") {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_ONBOARDING_BUILD=1 set; using existing onboarding dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    if env_truthy("ATO_DESKTOP_SKIP_ONBOARDING_BUILD") {
        println!(
            "cargo:warning=ATO_DESKTOP_SKIP_ONBOARDING_BUILD=1 set; onboarding dist check skipped"
        );
        return;
    }

    if !capsule_dir.join("node_modules").exists() {
        run_command(
            "npm",
            &["install"],
            &capsule_dir,
            "ato-onboarding npm install",
        );
    }
    run_command(
        "npm",
        &["run", "build"],
        &capsule_dir,
        "ato-onboarding vite build",
    );
    if entrypoint.exists() {
        return;
    }

    panic!(
        "ato-onboarding dist/index.html missing at {}. Set ATO_DESKTOP_SKIP_ONBOARDING_BUILD=1 to skip or run `npm run build` in assets/system/ato-onboarding/.",
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

    let entrypoint = dist_dir.join("index.html");

    if entrypoint.exists() {
        if env_truthy("ATO_DESKTOP_SKIP_DOCK_BUILD") {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_DOCK_BUILD=1 set; using existing dock dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    if env_truthy("ATO_DESKTOP_SKIP_DOCK_BUILD") {
        println!("cargo:warning=ATO_DESKTOP_SKIP_DOCK_BUILD=1 set; dock dist check skipped");
        return;
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

    if entrypoint.exists() {
        return;
    }

    panic!(
        "ato-dock dist/index.html missing at {} after build. Run `npm install && npm run build` in assets/system/ato-dock/.",
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
    let capsule_dir = manifest_dir.join("assets").join("system").join("ato-start");
    let dist_dir = capsule_dir.join("dist");
    let entrypoint = dist_dir.join("index.html");

    println!("cargo:rerun-if-changed={}", dist_dir.display());
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_START_BUILD");

    if entrypoint.exists() {
        if env_truthy("ATO_DESKTOP_SKIP_START_BUILD") {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_START_BUILD=1 set; using existing start dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    if env_truthy("ATO_DESKTOP_SKIP_START_BUILD") {
        println!("cargo:warning=ATO_DESKTOP_SKIP_START_BUILD=1 set; start dist check skipped");
        return;
    }

    if !capsule_dir.join("node_modules").exists() {
        run_command("npm", &["install"], &capsule_dir, "ato-start npm install");
    }
    run_command(
        "npm",
        &["run", "build"],
        &capsule_dir,
        "ato-start astro build",
    );
    if entrypoint.exists() {
        return;
    }

    panic!(
        "ato-start dist/index.html missing at {}. Set ATO_DESKTOP_SKIP_START_BUILD=1 to skip or run `npm run build` in assets/system/ato-start/.",
        dist_dir.display()
    );
}

fn check_store_dist(manifest_dir: &PathBuf) {
    let dist_dir = manifest_dir
        .join("assets")
        .join("system")
        .join("ato-store")
        .join("dist");
    let entrypoint = dist_dir.join("index.html");

    println!("cargo:rerun-if-changed={}", dist_dir.display());
    println!("cargo:rerun-if-env-changed=ATO_DESKTOP_SKIP_STORE_BUILD");

    if entrypoint.exists() {
        if env_truthy("ATO_DESKTOP_SKIP_STORE_BUILD") {
            println!(
                "cargo:warning=ATO_DESKTOP_SKIP_STORE_BUILD=1 set; using existing store dist at {}",
                dist_dir.display()
            );
        }
        return;
    }

    if env_truthy("ATO_DESKTOP_SKIP_STORE_BUILD") {
        println!("cargo:warning=ATO_DESKTOP_SKIP_STORE_BUILD=1 set; store dist check skipped");
        return;
    }

    panic!(
        "ato-store dist/index.html missing at {}. Set ATO_DESKTOP_SKIP_STORE_BUILD=1 to skip or build ato-web with `pnpm run build:desktop-store` and copy dist to assets/system/ato-store/dist/.",
        entrypoint.display()
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
