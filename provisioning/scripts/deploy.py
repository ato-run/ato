#!/usr/bin/env python3
"""
Rig Control Plane Deployment Script

This script sets up the Rig Control Plane on a fresh OCI VM.
It installs necessary packages, downloads the binary, configures services,
and runs database migrations.

Based on Phase 0 requirements from OCI_RIG_TODO_PHASE0.md
"""

import os
import subprocess
import sys
import tempfile
import shutil
import platform
import shlex
import importlib
import tarfile
from typing import Optional

BASE_DIR = os.path.dirname(os.path.abspath(__file__))
VENDOR_DIR = os.path.join(BASE_DIR, "_vendor")
if VENDOR_DIR not in sys.path:
    sys.path.insert(0, VENDOR_DIR)


def ensure_pip():
    """Ensure pip is available for the current interpreter."""
    try:
        subprocess.run(
            [sys.executable, '-m', 'pip', '--version'],
            check=True,
            capture_output=True,
            text=True,
        )
        return
    except (subprocess.CalledProcessError, FileNotFoundError):
        pass

    print("Bootstrapping pip using ensurepip...")
    try:
        subprocess.run([sys.executable, '-m', 'ensurepip', '--upgrade'], check=True)
    except subprocess.CalledProcessError:
        print("ensurepip failed; attempting to install python3-pip via apt...")
        subprocess.run('sudo apt update', shell=True, check=True)
        subprocess.run('sudo apt install -y python3-pip', shell=True, check=True)
    else:
        subprocess.run([sys.executable, '-m', 'pip', 'install', '--upgrade', 'pip'], check=False)


def ensure_python_package(module_name, package_name=None):
    """Import a module, installing it via pip if necessary."""
    try:
        return importlib.import_module(module_name)
    except ImportError:
        ensure_pip()
        package = package_name or module_name
        print(f"Installing {package}...")
        os.makedirs(VENDOR_DIR, exist_ok=True)
        subprocess.run([
            sys.executable,
            '-m',
            'pip',
            'install',
            '--upgrade',
            '--target',
            VENDOR_DIR,
            package,
        ], check=True)
        importlib.invalidate_caches()
        return importlib.import_module(module_name)


requests = ensure_python_package('requests')
toml = ensure_python_package('toml')


def youki_asset_candidates(version: str, arch: str) -> list[str]:
    arch = arch.lower()
    plain_version = version.lstrip('v')
    version_tokens = [version, plain_version]

    arch_alias_map = {
        'x86_64': ['x86_64', 'amd64'],
        'amd64': ['x86_64', 'amd64'],
        'aarch64': ['aarch64', 'arm64'],
        'arm64': ['aarch64', 'arm64'],
    }
    aliases = arch_alias_map.get(arch, [arch])

    candidates: list[str] = []

    def add(asset: str) -> None:
        if asset not in candidates:
            candidates.append(asset)

    for token in version_tokens:
        for alias in aliases:
            add(f"youki_{token}_linux_{alias}.tar.gz")
            add(f"youki_{token}_linux_{alias}.tar.xz")
            add(f"youki_{token}_{alias}-unknown-linux-gnu.tar.gz")
            add(f"youki_{token}_{alias}-unknown-linux-musl.tar.gz")
            add(f"youki_{token}_{alias}-unknown-linux-gnu.tar.xz")
            add(f"youki_{token}_{alias}-unknown-linux-musl.tar.xz")
            add(f"youki_{token}_{alias}.tar.gz")
    return candidates

def run_command(command, check=True, capture_output=True, shell=True):
    """Run a shell command with error handling."""
    try:
        result = subprocess.run(
            command,
            shell=shell,
            check=check,
            capture_output=capture_output,
            text=True
        )
        if capture_output:
            return result.stdout.strip(), result.stderr.strip()
        return result
    except subprocess.CalledProcessError as e:
        print(f"Command failed: {command}")
        print(f"Error: {e}")
        if capture_output:
            print(f"stdout: {e.stdout}")
            print(f"stderr: {e.stderr}")
        raise

def detect_architecture():
    """Detect system architecture."""
    arch = platform.machine()
    print(f"Detected architecture: {arch}")
    if arch not in ['aarch64', 'arm64', 'x86_64', 'amd64']:
        print(f"Warning: Unsupported architecture {arch}")
    return arch

def install_build_tools():
    """Install build-essential and development dependencies (Phase 0 - 0.5.5)."""
    print("\n=== Installing build tools and dependencies ===")
    packages = [
        'build-essential',
        'pkg-config',
        'libssl-dev',
        'git',
        'wget',
        'curl',
        'python3-pip',
        'python3-venv',
        'sqlite3',
        'libsqlite3-dev',
        'lvm2',
        'cryptsetup',
        'debian-keyring',
        'debian-archive-keyring',
        'apt-transport-https'
    ]
    print("Updating package list...")
    run_command('sudo apt update')
    print(f"Installing packages: {', '.join(packages)}")
    run_command(f"sudo apt install -y {' '.join(packages)}")
    print("✓ Build tools installed")

def install_youki():
    """Install youki OCI runtime (Phase 0 - 0.5.1)."""
    print("\n=== Installing youki ===")
    arch = detect_architecture()
    version = 'v0.3.3'
    repo = 'youki-dev/youki'

    aliases = {
        'x86_64': ['x86_64', 'amd64'],
        'amd64': ['x86_64', 'amd64'],
        'aarch64': ['aarch64', 'arm64'],
        'arm64': ['aarch64', 'arm64'],
    }
    arch_aliases = aliases.get(arch, [arch])

    api_url = f'https://api.github.com/repos/{repo}/releases/tags/{version}'
    headers = {}
    token = os.environ.get('GITHUB_TOKEN') or os.environ.get('GH_TOKEN')
    if token:
        headers['Authorization'] = f'token {token}'

    print(f"Fetching youki release metadata: {api_url}")
    try:
        response = requests.get(api_url, headers=headers, timeout=30)
        response.raise_for_status()
        release_data = response.json()
        assets = release_data.get('assets', [])
    except requests.RequestException as err:
        print(f"Warning: Failed to fetch youki release metadata ({err}).")
        assets = []

    selected_asset: Optional[dict] = None
    if assets:
        def asset_priority(asset: dict) -> int:
            name = asset.get('name', '').lower()
            priority = 100
            if name.endswith('.tar.gz'):
                priority = 0
            elif name.endswith('.tar.xz'):
                priority = 1
            return priority

        sorted_assets = sorted(assets, key=asset_priority)
        for asset in sorted_assets:
            name = asset.get('name', '').lower()
            if not name:
                continue
            if not any(alias in name for alias in arch_aliases):
                continue
            if not (name.endswith('.tar.gz') or name.endswith('.tar.xz')):
                continue
            selected_asset = asset
            break

    if selected_asset:
        download_url = selected_asset.get('browser_download_url')
        print(f"Selected youki asset: {selected_asset.get('name')}\nURL: {download_url}")
        _install_youki_from_url(download_url)
    else:
        print("Warning: GitHub release does not provide a matching youki binary for this architecture.")
        print("Attempting to install youki via apt if available...")
        try:
            run_command('sudo apt update')
            run_command('sudo apt install -y youki')
        except Exception as err:
            raise RuntimeError(
                "Failed to obtain youki binary automatically. "
                "Please install youki manually and re-run the deployment."
            ) from err

    stdout, _ = run_command('youki --version')
    print(f"✓ youki installed: {stdout}")


def _install_youki_from_url(url: str) -> None:
    with tempfile.TemporaryDirectory() as tmpdir:
        asset_name = url.rsplit('/', 1)[-1]
        tarball_path = os.path.join(tmpdir, asset_name)

        response = requests.get(url, stream=True, timeout=60)
        response.raise_for_status()

        with open(tarball_path, 'wb') as f:
            for chunk in response.iter_content(chunk_size=8192):
                f.write(chunk)

        print("Extracting youki archive...")
        with tarfile.open(tarball_path, 'r:*') as archive:
            archive.extractall(tmpdir)

        youki_binary = None
        for root_dir, _dirs, files in os.walk(tmpdir):
            if 'youki' in files:
                youki_binary = os.path.join(root_dir, 'youki')
                break

        if not youki_binary:
            raise RuntimeError("youki binary not found in extracted archive")

        print("Installing youki to /usr/local/bin/")
        run_command(f'sudo mv {shlex.quote(youki_binary)} /usr/local/bin/youki')
        run_command('sudo chmod +x /usr/local/bin/youki')

def install_rust():
    """Install Rust toolchain (Phase 0 - 0.5.3)."""
    print("\n=== Installing Rust toolchain ===")
    
    # Check if already installed
    try:
        stdout, _ = run_command('rustc --version', check=False)
        if 'rustc' in stdout:
            print(f"✓ Rust already installed: {stdout}")
            return
    except:
        pass
    
    print("Downloading Rust installer...")
    rustup_url = 'https://sh.rustup.rs'
    response = requests.get(rustup_url)
    response.raise_for_status()
    
    with tempfile.NamedTemporaryFile(mode='w', delete=False, suffix='.sh') as f:
        f.write(response.text)
        installer_path = f.name
    
    try:
        print("Running Rust installer...")
        run_command(f'sh {installer_path} -y', capture_output=False)
        
        # Source cargo env
        cargo_env = os.path.expanduser('~/.cargo/env')
        if os.path.exists(cargo_env):
            run_command(f'. {cargo_env}')
        
        # Update PATH for current session
        cargo_bin = os.path.expanduser('~/.cargo/bin')
        if cargo_bin not in os.environ['PATH']:
            os.environ['PATH'] = f"{cargo_bin}:{os.environ['PATH']}"
        
        # Verify installation
        stdout, _ = run_command('~/.cargo/bin/rustc --version')
        print(f"✓ Rust installed: {stdout}")
    finally:
        os.unlink(installer_path)

def install_caddy():
    """Install Caddy web server from official repository (Phase 0 - 0.5.2)."""
    print("\n=== Installing Caddy ===")
    
    # Check if already installed
    try:
        stdout, _ = run_command('caddy version', check=False)
        if 'caddy' in stdout.lower():
            print(f"✓ Caddy already installed: {stdout}")
            return
    except:
        pass
    
    print("Adding Caddy repository...")

    if shutil.which('gpg') is None:
        print("Installing gnupg for key management...")
        run_command('sudo apt install -y gnupg', capture_output=False)
    
    # Add GPG key
    keyring_path = '/usr/share/keyrings/caddy-stable-archive-keyring.gpg'
    run_command(
        "curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | "
        "gpg --dearmor --batch --yes | "
        "sudo tee /usr/share/keyrings/caddy-stable-archive-keyring.gpg > /dev/null",
        capture_output=False,
    )
    run_command(f'sudo chmod 0644 {keyring_path}')
    
    # Add repository
    run_command(
        "curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | "
        "sudo tee /etc/apt/sources.list.d/caddy-stable.list"
    )
    
    # Install Caddy
    print("Installing Caddy...")
    run_command('sudo apt update')
    run_command('sudo apt install -y caddy')
    
    # Verify installation
    stdout, _ = run_command('caddy version')
    print(f"✓ Caddy installed: {stdout}")

def setup_firewall():
    """Configure UFW firewall (Phase 0 - 0.2)."""
    print("\n=== Configuring UFW firewall ===")
    
    # Check if UFW is installed
    try:
        run_command('which ufw', check=True)
    except:
        print("Installing UFW...")
        run_command('sudo apt install -y ufw')
    
    print("Configuring firewall rules...")
    run_command('sudo ufw default deny incoming')
    run_command('sudo ufw allow 80/tcp')   # HTTP for ACME
    run_command('sudo ufw allow 443/tcp')  # HTTPS
    # Allow Tailscale tunnel traffic (interface may not exist yet but the rule will persist)
    run_command('sudo ufw allow in on tailscale0', check=False)
    run_command('sudo ufw allow out on tailscale0', check=False)
    run_command('sudo ufw --force enable')
    
    stdout, _ = run_command('sudo ufw status')
    print("✓ Firewall configured:")
    print(stdout)


def install_cuda_toolkit():
    """Install CUDA toolkit to provide nvcc compiler."""
    print("\n=== Installing CUDA toolkit (nvcc) ===")

    try:
        stdout, _ = run_command('nvcc --version', check=False)
        if stdout:
            print("✓ nvcc already installed:")
            print(stdout.splitlines()[0])
            return
    except Exception:
        pass

    try:
        run_command('sudo apt install -y nvidia-cuda-toolkit')
        stdout, _ = run_command('nvcc --version', check=False)
        if stdout:
            print(f"✓ CUDA toolkit installed: {stdout.splitlines()[0]}")
        else:
            print("⚠️  CUDA toolkit installed but nvcc version could not be determined.")
    except Exception as err:
        print(f"Warning: Failed to install CUDA toolkit automatically ({err}).")
        print("         Please install nvcc manually if GPU workloads require it.")


def install_tailscale():
    """Install Tailscale to provide secure connectivity to the rig."""
    print("\n=== Installing Tailscale ===")

    try:
        stdout, _ = run_command('tailscale version', check=False)
        if stdout:
            print(f"✓ Tailscale already installed: {stdout}")
            return
    except Exception:
        pass

    print("Fetching installer from tailscale.com...")
    run_command('curl -fsSL https://tailscale.com/install.sh | sh', capture_output=False)

    stdout, _ = run_command('tailscale version')
    print(f"✓ Tailscale installed: {stdout}")


def configure_tailscale(auth_key=None, hostname=None, login_server=None):
    """Enable tailscaled and optionally connect the node to a tailnet."""
    print("\n=== Configuring Tailscale ===")

    run_command('sudo systemctl enable tailscaled', check=False)
    run_command('sudo systemctl start tailscaled')

    if not hostname:
        detected = platform.node() or 'rig'
        hostname = f"onescluster-{detected}"

    cmd_parts = ["sudo tailscale up"]

    if auth_key:
        cmd_parts.append(f"--authkey {shlex.quote(auth_key)}")
    if hostname:
        cmd_parts.append(f"--hostname {shlex.quote(hostname)}")
    if login_server:
        cmd_parts.append(f"--login-server {shlex.quote(login_server)}")

    if auth_key:
        command = ' '.join(cmd_parts)
        print(f"Bringing tailscale interface online with hostname {hostname}...")
        run_command(command, capture_output=False)
        status_stdout, _ = run_command('tailscale status --peers', check=False)
        print("✓ Tailscale connection established (status --peers):")
        if status_stdout:
            print(status_stdout)
    else:
        print("⚠️  Skipping `tailscale up` because no auth key was provided.")
        print("   Export TAILSCALE_AUTHKEY (and optional TAILSCALE_LOGIN_SERVER/TAILSCALE_HOSTNAME) before rerunning,")
        print("   or run `sudo tailscale up --authkey <KEY>` manually after deployment.")


def download_binary():
    """Download the rig-manager binary from GitHub Releases."""
    print("\n=== Downloading Control Plane binary ===")

    # TODO: Replace with actual GitHub release URL
    url = 'https://github.com/your-org/rig-manager/releases/latest/download/rig-manager'
    binary_path = '/usr/local/bin/rig-manager'

    print(f"Downloading binary from {url}")
    try:
        response = requests.get(url, stream=True)
        response.raise_for_status()
        
        with tempfile.NamedTemporaryFile(delete=False) as f:
            for chunk in response.iter_content(chunk_size=8192):
                f.write(chunk)
            temp_path = f.name
        
        print(f"Installing binary to {binary_path}")
        run_command(f'sudo mv {temp_path} {binary_path}')
        run_command(f'sudo chmod +x {binary_path}')
        print("✓ Control Plane binary installed")
    except Exception as e:
        print(f"Warning: Failed to download binary: {e}")
        print("You may need to build and deploy manually")

def copy_config():
    """Copy configuration file to system location."""
    print("\n=== Copying configuration ===")
    config_src = 'config/production.toml'
    config_dst = '/etc/rig-manager/production.toml'

    if not os.path.exists(config_src):
        print(f"Warning: Config file {config_src} not found, skipping")
        return

    print(f"Copying config from {config_src} to {config_dst}")
    run_command('sudo mkdir -p /etc/rig-manager')
    run_command(f'sudo cp {config_src} {config_dst}')
    print("✓ Configuration copied")

def setup_systemd():
    """Copy and enable systemd service files."""
    print("\n=== Setting up systemd services ===")
    
    # Use manage_services.py to generate and install services
    script_dir = os.path.dirname(os.path.abspath(__file__))
    manage_services = os.path.join(script_dir, 'manage_services.py')
    
    if not os.path.exists(manage_services):
        print("Warning: manage_services.py not found, using legacy method")
        # Fallback to legacy method
        services = ['rig-manager.service']
        systemd_dir = os.path.join(os.path.dirname(script_dir), 'systemd')
        
        for service in services:
            src = os.path.join(systemd_dir, service)
            dst = f'/etc/systemd/system/{service}'
            
            if not os.path.exists(src):
                print(f"Warning: Service file {src} not found, skipping")
                continue
                
            print(f"Copying service file {service}")
            run_command(f'sudo cp {src} {dst}')
        
        print("Reloading systemd daemon")
        run_command('sudo systemctl daemon-reload')
        
        for service in services:
            src = os.path.join(systemd_dir, service)
            if os.path.exists(src):
                print(f"Enabling service {service}")
                run_command(f'sudo systemctl enable {service}')
    else:
        print("Using manage_services.py to generate and install services")
        # Generate services
        python_bin = sys.executable
        run_command(f'{python_bin} {manage_services} generate --output-dir provisioning/systemd')
        
        # Install basic services
        basic_services = ['rig-manager.service', 'caddy.service']
        run_command(f'{python_bin} {manage_services} install --services {" ".join(basic_services)}')
        
        # Enable services
        run_command(f'{python_bin} {manage_services} enable --services {" ".join(basic_services)}')
    
    print("✓ Systemd services configured")

def install_sqlx_cli():
    """Install sqlx-cli for database migrations."""
    print("\n=== Installing sqlx-cli ===")
    
    # Check if already installed
    try:
        stdout, _ = run_command('~/.cargo/bin/sqlx --version', check=False)
        if 'sqlx' in stdout:
            print(f"✓ sqlx-cli already installed: {stdout}")
            return
    except:
        pass
    
    print("Installing sqlx-cli (this may take several minutes)...")
    cargo_bin = os.path.expanduser('~/.cargo/bin/cargo')
    run_command(f'{cargo_bin} install sqlx-cli --no-default-features --features sqlite')
    
    stdout, _ = run_command('~/.cargo/bin/sqlx --version')
    print(f"✓ sqlx-cli installed: {stdout}")

def setup_database():
    """Create database directory and run migrations."""
    print("\n=== Setting up database ===")
    
    db_dir = 'db'
    db_path = os.path.join(db_dir, 'control_plane.db')
    
    # Create db directory
    if not os.path.exists(db_dir):
        print(f"Creating database directory: {db_dir}")
        os.makedirs(db_dir)
    
    # Check if migrations directory exists
    migrations_dir = os.path.join(db_dir, 'migrations')
    if not os.path.exists(migrations_dir):
        print(f"Warning: Migrations directory {migrations_dir} not found, skipping")
        return
    
    # Run migrations
    db_url = f'sqlite://{os.path.abspath(db_path)}'
    print(f"Running migrations on {db_url}")
    sqlx_bin = os.path.expanduser('~/.cargo/bin/sqlx')
    run_command(f'{sqlx_bin} migrate run --database-url {db_url}')
    print("✓ Database migrations completed")

def run_verification_tests():
    """Run verification tests to ensure all components are installed correctly."""
    print("\n=== Running verification tests ===")
    
    tests = [
        ('youki', 'youki --version'),
        ('Caddy', 'caddy version'),
        ('Rust', '~/.cargo/bin/rustc --version'),
        ('Cargo', '~/.cargo/bin/cargo --version'),
        ('SQLite', 'sqlite3 --version'),
        ('Git', 'git --version'),
    ('Tailscale', 'tailscale status --peers'),
    ('CUDA (nvcc)', 'nvcc --version'),
    ]
    
    failed = []
    for name, command in tests:
        try:
            stdout, _ = run_command(command)
            print(f"✓ {name}: {stdout.split()[0] if stdout else 'OK'}")
        except Exception as e:
            print(f"✗ {name}: FAILED")
            failed.append(name)
    
    if failed:
        print(f"\n⚠️  Some components failed verification: {', '.join(failed)}")
    else:
        print("\n✅ All components verified successfully!")
    
    return len(failed) == 0

def main():
    """Main deployment function."""
    try:
        print("=" * 60)
        print("Rig Control Plane - Phase 0 Setup")
        print("=" * 60)
        
        arch = detect_architecture()
        if arch not in ['aarch64', 'arm64']:
            response = input(f"Architecture {arch} may not be supported. Continue? (y/N): ")
            if response.lower() != 'y':
                print("Setup cancelled")
                sys.exit(0)
        
        # Phase 0 - 0.5.5: Build tools and dependencies
        install_build_tools()

        # CUDA toolkit (nvcc)
        install_cuda_toolkit()
        
        # Phase 0 - 0.5.1: youki
        install_youki()
        
        # Phase 0 - 0.5.3: Rust toolchain
        install_rust()
        
        # Phase 0 - 0.5.2: Caddy
        install_caddy()
        
        # Phase 0 - 0.2: Firewall
        setup_firewall()

        # Secure networking via Tailscale
        install_tailscale()
        tailscale_auth_key = os.environ.get('TAILSCALE_AUTHKEY') or os.environ.get('TAILSCALE_AUTH_KEY')
        tailscale_hostname = os.environ.get('TAILSCALE_HOSTNAME')
        tailscale_login_server = os.environ.get('TAILSCALE_LOGIN_SERVER')
        configure_tailscale(tailscale_auth_key, tailscale_hostname, tailscale_login_server)
        
        # Control Plane specific setup
        download_binary()
        copy_config()
        setup_systemd()
        install_sqlx_cli()
        setup_database()
        
        # Verification
        verification_passed = run_verification_tests()
        
        print("\n" + "=" * 60)
        if verification_passed:
            print("✅ Deployment completed successfully!")
        else:
            print("⚠️  Deployment completed with warnings")
        print("=" * 60)
        
        print("\nNext steps:")
        print("1. Verify Tailscale status: tailscale status --peers")
        print("2. Review firewall settings: sudo ufw status")
        print("3. Check Caddy status: sudo systemctl status caddy")
        print("4. Start Control Plane: sudo systemctl start rig-manager")
        print("5. View logs: sudo journalctl -u rig-manager -f")

    except KeyboardInterrupt:
        print("\n\nSetup interrupted by user")
        sys.exit(1)
    except Exception as e:
        print(f"\n❌ Deployment failed: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)

if __name__ == '__main__':
    main()
