#!/usr/bin/env python3
"""
Systemd Service Generator

Generates all necessary systemd service files for Rig Control Plane.
This replaces the systemd/ directory with dynamic service generation.
"""

import os
import sys
from pathlib import Path

# Service templates
SERVICES = {
    'caddy.service': """[Unit]
Description=Caddy
Documentation=https://caddyserver.com/docs/
After=network.target network-online.target
Requires=network-online.target

[Service]
Type=notify
User=caddy
Group=caddy
ExecStart=/usr/bin/caddy run --environ --config /etc/caddy/Caddyfile
ExecReload=/usr/bin/caddy reload --config /etc/caddy/Caddyfile
TimeoutStopSec=5s
LimitNOFILE=1048576
LimitNPROC=512
PrivateTmp=true
ProtectSystem=full
AmbientCapabilities=CAP_NET_BIND_SERVICE
Restart=on-failure

[Install]
WantedBy=multi-user.target
""",
    
    'rig-manager.service': """[Unit]
Description=Rig Manager
After=network.target caddy.service
Requires=caddy.service

[Service]
Type=simple
User=rig
Group=rig
EnvironmentFile=-/etc/rig-manager.env
ExecStart=/usr/local/bin/rig-manager serve
Restart=always
RestartSec=5

# Security
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/rig
ProtectHome=yes

[Install]
WantedBy=multi-user.target
""",
    
    'rig-reconciler.service': """[Unit]
Description=Rig Container Reconciler
After=rig-manager.service

[Service]
Type=oneshot
User=rig
Group=rig
ExecStart=/usr/local/bin/rig-manager reconcile
EnvironmentFile=-/etc/rig-manager.env

# Security
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/rig
""",
    
    'rig-reconciler.timer': """[Unit]
Description=Run Rig Reconciler every 5 minutes

[Timer]
OnBootSec=2min
OnUnitActiveSec=5min

[Install]
WantedBy=timers.target
""",
    
    'rig-cleanup.service': """[Unit]
Description=Rig Cleanup Service
After=rig-manager.service

[Service]
Type=oneshot
User=rig
Group=rig
ExecStart=/usr/local/bin/rig-manager cleanup
EnvironmentFile=-/etc/rig-manager.env

# Security
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/rig
""",
    
    'rig-cleanup.timer': """[Unit]
Description=Run Rig Cleanup daily

[Timer]
OnBootSec=10min
OnCalendar=daily

[Install]
WantedBy=timers.target
"""
}

def generate_service(name, content, output_dir=None):
    """Generate a single service file."""
    if output_dir is None:
        output_dir = Path.cwd()
    else:
        output_dir = Path(output_dir)
    
    output_dir.mkdir(parents=True, exist_ok=True)
    service_path = output_dir / name
    
    with open(service_path, 'w') as f:
        f.write(content.lstrip())
    
    print(f"✓ Generated {service_path}")
    return service_path

def install_services(services_to_install=None):
    """Install service files to /etc/systemd/system/."""
    import subprocess
    
    if services_to_install is None:
        services_to_install = SERVICES.keys()
    
    print("\n=== Installing systemd services ===")
    
    for service_name in services_to_install:
        if service_name not in SERVICES:
            print(f"Warning: Unknown service {service_name}")
            continue
        
        content = SERVICES[service_name]
        
        # Write to temp file first
        import tempfile
        with tempfile.NamedTemporaryFile(mode='w', delete=False, suffix=f'.{service_name}') as f:
            f.write(content.lstrip())
            temp_path = f.name
        
        try:
            # Copy to systemd directory
            dest_path = f'/etc/systemd/system/{service_name}'
            subprocess.run(['sudo', 'cp', temp_path, dest_path], check=True)
            print(f"✓ Installed {service_name}")
        finally:
            os.unlink(temp_path)
    
    # Reload systemd
    print("\nReloading systemd daemon...")
    subprocess.run(['sudo', 'systemctl', 'daemon-reload'], check=True)
    print("✓ Systemd daemon reloaded")

def enable_services(services_to_enable=None):
    """Enable services to start on boot."""
    import subprocess
    
    if services_to_enable is None:
        # Default services to enable
        services_to_enable = [
            'rig-manager.service',
            'caddy.service',
            'rig-reconciler.timer',
            'rig-cleanup.timer'
        ]
    
    print("\n=== Enabling services ===")
    
    for service_name in services_to_enable:
        try:
            subprocess.run(['sudo', 'systemctl', 'enable', service_name], check=True)
            print(f"✓ Enabled {service_name}")
        except subprocess.CalledProcessError as e:
            print(f"✗ Failed to enable {service_name}: {e}")

def list_services():
    """List all available service templates."""
    print("\n=== Available systemd services ===")
    for i, service_name in enumerate(SERVICES.keys(), 1):
        service_type = "Timer" if service_name.endswith('.timer') else "Service"
        print(f"{i}. {service_name:<30} ({service_type})")

def main():
    """Main function."""
    import argparse
    
    parser = argparse.ArgumentParser(
        description='Generate and manage systemd services for Rig Control Plane'
    )
    parser.add_argument(
        'action',
        choices=['generate', 'install', 'enable', 'list', 'all'],
        help='Action to perform'
    )
    parser.add_argument(
        '--output-dir',
        help='Output directory for generated files (default: provisioning/systemd/)'
    )
    parser.add_argument(
        '--services',
        nargs='+',
        help='Specific services to process (default: all)'
    )
    
    args = parser.parse_args()
    
    if args.action == 'list':
        list_services()
        return
    
    # Determine output directory
    if args.output_dir:
        output_dir = args.output_dir
    else:
        output_dir = 'provisioning/systemd'
    
    # Determine which services to process
    services = args.services if args.services else list(SERVICES.keys())
    
    if args.action == 'generate':
        print(f"\n=== Generating services to {output_dir} ===")
        for service_name in services:
            if service_name in SERVICES:
                generate_service(service_name, SERVICES[service_name], output_dir)
        print(f"\n✅ Generated {len(services)} service file(s)")
        
    elif args.action == 'install':
        install_services(services)
        
    elif args.action == 'enable':
        enable_services(services)
        
    elif args.action == 'all':
        # Generate, install, and enable
        print(f"\n=== Generating services to {output_dir} ===")
        for service_name in services:
            if service_name in SERVICES:
                generate_service(service_name, SERVICES[service_name], output_dir)
        
        install_services(services)
        enable_services(services)
        
        print("\n✅ All services generated, installed, and enabled!")

if __name__ == '__main__':
    main()
