#!/usr/bin/env python3
"""
Rig Manager Cleanup Script

This script tears down the Rig Manager, restoring the system
to a clean state suitable for re-provisioning.

Reverses all operations performed by deploy.py including:
- Stopping services
- Removing containers and volumes
- Uninstalling binaries (youki, rig-manager)
- Removing Rust toolchain
- Cleaning up configurations
- Optionally uninstalling packages
"""

import os
import subprocess
import sys
import sqlite3
import shutil
import glob

def run_command(command, check=True, capture_output=True):
    """Run a shell command with error handling."""
    try:
        result = subprocess.run(
            command,
            shell=True,
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

def confirm_destruction():
    """Prompt user for confirmation before destructive operations."""
    print("WARNING: This script will destroy all data and reset the system!")
    print("This includes stopping services, deleting containers, destroying storage,")
    print("and removing configuration files.")
    print()
    response = input("Type 'DESTROY' to confirm: ")
    if response != 'DESTROY':
        print("Cleanup aborted.")
        sys.exit(0)

def stop_services():
    """Stop and disable systemd services."""
    print("\n=== Stopping and disabling services ===")
    services = ['rig-manager.service', 'caddy.service']

    for service in services:
        print(f"Stopping service {service}")
        try:
            run_command(f'sudo systemctl stop {service}', check=False)
        except:
            print(f"  Note: Service {service} may not be running")

        print(f"Disabling service {service}")
        try:
            run_command(f'sudo systemctl disable {service}', check=False)
        except:
            print(f"  Note: Service {service} may not be enabled")
    
    print("✓ Services stopped and disabled")

def stop_containers():
    """Stop all running capsules/containers."""
    print("\n=== Stopping containers ===")
    db_path = 'db/control_plane.db'
    if not os.path.exists(db_path):
        print("Database not found, skipping container cleanup")
        return

    try:
        conn = sqlite3.connect(db_path)
        cursor = conn.cursor()
        cursor.execute("SELECT id FROM capsules WHERE status = 'running'")
        running_capsules = cursor.fetchall()
        conn.close()

        if not running_capsules:
            print("No running capsules found")
        else:
            for (capsule_id,) in running_capsules:
                print(f"Stopping capsule {capsule_id}")
                try:
                    run_command(f'sudo runc kill {capsule_id} KILL', check=False)
                    run_command(f'sudo runc delete {capsule_id}', check=False)
                except:
                    print(f"  Warning: Failed to stop capsule {capsule_id}")

    except Exception as e:
        print(f"Error accessing database: {e}")
        # Fallback: try to list and kill all runc containers
        try:
            stdout, _ = run_command('sudo runc list', check=False)
            lines = stdout.split('\n')[1:] if stdout else []
            if lines:
                print("Attempting fallback container cleanup...")
                for line in lines:
                    if line.strip():
                        container_id = line.split()[0]
                        print(f"  Stopping container {container_id}")
                        run_command(f'sudo runc kill {container_id} KILL', check=False)
                        run_command(f'sudo runc delete {container_id}', check=False)
        except:
            print("  Warning: Fallback container cleanup failed")
    
    print("✓ Container cleanup completed")

def destroy_storage():
    """Destroy all LVM volumes and encrypted storage."""
    print("\n=== Destroying storage ===")
    db_path = 'db/control_plane.db'
    if not os.path.exists(db_path):
        print("Database not found, skipping storage cleanup")
        return

    try:
        conn = sqlite3.connect(db_path)
        cursor = conn.cursor()
        cursor.execute("SELECT name, mount_path FROM volumes")
        volumes = cursor.fetchall()
        
        if not volumes:
            print("No volumes found")
        else:
            for name, mount_path in volumes:
                print(f"Destroying volume {name}")
                try:
                    # Unmount
                    run_command(f'sudo umount {mount_path}', check=False)
                    # Close LUKS
                    run_command(f'sudo cryptsetup luksClose {name}', check=False)
                    # Remove LV
                    run_command(f'sudo lvremove -f {name}', check=False)
                    # Remove directory
                    if os.path.exists(mount_path):
                        run_command(f'sudo rmdir {mount_path}', check=False)
                except Exception as e:
                    print(f"  Warning: Error destroying volume {name}: {e}")

            # Remove encryption keys from DB
            cursor.execute("DELETE FROM volumes")
            conn.commit()
        
        conn.close()
        print("✓ Storage cleanup completed")

    except Exception as e:
        print(f"Error accessing database for storage cleanup: {e}")

def remove_files():
    """Remove binaries, configs, and data files."""
    print("\n=== Removing files and configurations ===")
    
    # Binaries
    binaries = [
        '/usr/local/bin/rig-manager',
        '/usr/local/bin/youki',
    ]
    
    for binary in binaries:
        if os.path.exists(binary):
            print(f"Removing {binary}")
            try:
                run_command(f'sudo rm -f {binary}')
            except Exception as e:
                print(f"  Warning: Error removing {binary}: {e}")
    
    # Configuration directories
    config_dirs = [
        '/etc/rig-manager',
        '/var/log/rig',
    ]
    
    for dir_path in config_dirs:
        if os.path.exists(dir_path):
            print(f"Removing directory {dir_path}")
            try:
                run_command(f'sudo rm -rf {dir_path}')
            except Exception as e:
                print(f"  Warning: Error removing {dir_path}: {e}")
    
    # Database
    if os.path.exists('db/control_plane.db'):
        print("Removing database db/control_plane.db")
        try:
            os.remove('db/control_plane.db')
        except Exception as e:
            print(f"  Warning: Error removing database: {e}")
    
    # systemd service files
    service_files = [
        '/etc/systemd/system/rig-manager.service',
        '/etc/systemd/system/caddy.service'
    ]

    for service_file in service_files:
        if os.path.exists(service_file):
            print(f"Removing {service_file}")
            try:
                run_command(f'sudo rm -f {service_file}')
            except Exception as e:
                print(f"  Warning: Error removing {service_file}: {e}")

    print("Reloading systemd daemon")
    run_command('sudo systemctl daemon-reload', check=False)
    print("✓ Files and configurations removed")

def remove_rust():
    """Remove Rust toolchain."""
    print("\n=== Removing Rust toolchain ===")
    
    rust_home = os.path.expanduser('~/.cargo')
    rustup_home = os.path.expanduser('~/.rustup')
    
    if os.path.exists(rust_home) or os.path.exists(rustup_home):
        response = input("Remove Rust toolchain? This will delete ~/.cargo and ~/.rustup (y/N): ")
        if response.lower() == 'y':
            if os.path.exists(rust_home):
                print(f"Removing {rust_home}")
                try:
                    shutil.rmtree(rust_home)
                except Exception as e:
                    print(f"  Warning: Error removing Rust: {e}")
            
            if os.path.exists(rustup_home):
                print(f"Removing {rustup_home}")
                try:
                    shutil.rmtree(rustup_home)
                except Exception as e:
                    print(f"  Warning: Error removing rustup: {e}")
            
            print("✓ Rust toolchain removed")
        else:
            print("  Skipping Rust removal")
    else:
        print("  Rust toolchain not found")

def remove_caddy_repo():
    """Remove Caddy repository."""
    print("\n=== Removing Caddy repository ===")
    
    caddy_list = '/etc/apt/sources.list.d/caddy-stable.list'
    caddy_key = '/usr/share/keyrings/caddy-stable-archive-keyring.gpg'
    
    removed_any = False
    if os.path.exists(caddy_list):
        print(f"Removing {caddy_list}")
        try:
            run_command(f'sudo rm -f {caddy_list}')
            removed_any = True
        except Exception as e:
            print(f"  Warning: Error removing Caddy repo list: {e}")
    
    if os.path.exists(caddy_key):
        print(f"Removing {caddy_key}")
        try:
            run_command(f'sudo rm -f {caddy_key}')
            removed_any = True
        except Exception as e:
            print(f"  Warning: Error removing Caddy GPG key: {e}")
    
    if removed_any:
        print("✓ Caddy repository removed")
    else:
        print("  Caddy repository not found")

def reset_firewall():
    """Reset UFW firewall to default state."""
    print("\n=== Resetting firewall ===")
    
    try:
        run_command('which ufw', check=False)
        
        response = input("Reset firewall to default (deny all)? This will disable UFW (y/N): ")
        if response.lower() == 'y':
            print("Disabling UFW")
            run_command('sudo ufw --force disable', check=False)
            print("Resetting UFW rules")
            run_command('sudo ufw --force reset', check=False)
            print("✓ Firewall reset")
        else:
            print("  Skipping firewall reset")
    except:
        print("  UFW not installed, skipping")

def uninstall_packages():
    """Uninstall installed packages (optional)."""
    print("\n=== Uninstalling packages ===")
    
    response = input("Uninstall all packages (youki, caddy, lvm2, cryptsetup, etc.)? (y/N): ")
    if response.lower() != 'y':
        print("  Skipping package uninstallation")
        return
    
    packages = [
        'caddy',
        'lvm2',
        'cryptsetup',
        'build-essential',
        'pkg-config',
        'libssl-dev',
    ]
    
    print(f"Uninstalling packages: {', '.join(packages)}")
    try:
        run_command(f"sudo apt purge --auto-remove -y {' '.join(packages)}", check=False)
        print("✓ Packages uninstalled")
    except Exception as e:
        print(f"  Warning: Error uninstalling packages: {e}")

def main():
    """Main cleanup function."""
    try:
        print("=" * 60)
        print("Rig Manager - Cleanup Script")
        print("=" * 60)

        confirm_destruction()

        stop_services()
        stop_containers()
        destroy_storage()
        remove_files()
        remove_rust()
        remove_caddy_repo()
        reset_firewall()
        uninstall_packages()

        print("\n" + "=" * 60)
        print("✅ Cleanup completed successfully!")
        print("=" * 60)
        print("\nThe system has been restored to a clean state.")
        print("You can now re-run deploy.py if needed.")

    except KeyboardInterrupt:
        print("\n\nCleanup interrupted by user")
        sys.exit(1)
    except Exception as e:
        print(f"\n❌ Cleanup failed: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)

if __name__ == '__main__':
    main()
