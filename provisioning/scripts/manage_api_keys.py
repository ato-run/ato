#!/usr/bin/env python3
"""
API Key Generator and Manager

Generates and manages API keys for Rig Control Plane.
This replaces scripts/generate_api_key.sh with a Python implementation.
"""

import os
import sys
import hashlib
import secrets
import base64
from pathlib import Path

def generate_api_key(length=32):
    """Generate a random API key."""
    # Generate random bytes and encode as base64
    random_bytes = secrets.token_bytes(length)
    api_key = base64.b64encode(random_bytes).decode('utf-8')
    return api_key

def hash_api_key(api_key):
    """Calculate SHA256 hash of API key."""
    return hashlib.sha256(api_key.encode('utf-8')).hexdigest()

def save_api_key(api_key, api_key_hash, env_file='.env', key_file='api_key.txt'):
    """Save API key and hash to files."""
    # Save hash to .env
    env_path = Path(env_file)
    
    # Read existing .env if it exists
    env_lines = []
    if env_path.exists():
        with open(env_path, 'r') as f:
            env_lines = [line for line in f if not line.startswith('RIG_API_KEY_HASH=')]
    
    # Add new hash
    env_lines.append(f'RIG_API_KEY_HASH={api_key_hash}\n')
    
    with open(env_path, 'w') as f:
        f.writelines(env_lines)
    
    print(f"✅ Saved hash to {env_file}")
    
    # Save API key to separate file
    key_path = Path(key_file)
    with open(key_path, 'w') as f:
        f.write(api_key)
    
    # Set restrictive permissions
    os.chmod(key_path, 0o600)
    
    print(f"✅ Saved API key to {key_file}")
    print()
    print("⚠️  IMPORTANT:")
    print(f"   - Keep {key_file} secure and don't commit it to version control!")
    print("   - The client needs the actual API key, not the hash.")
    print(f"   - Add '{key_file}' to your .gitignore")

def verify_api_key(api_key, stored_hash):
    """Verify an API key against a stored hash."""
    calculated_hash = hash_api_key(api_key)
    return calculated_hash == stored_hash

def load_api_key_hash(env_file='.env'):
    """Load API key hash from .env file."""
    env_path = Path(env_file)
    if not env_path.exists():
        return None
    
    with open(env_path, 'r') as f:
        for line in f:
            if line.startswith('RIG_API_KEY_HASH='):
                return line.strip().split('=', 1)[1]
    
    return None

def main():
    """Main function."""
    import argparse
    
    parser = argparse.ArgumentParser(
        description='Generate and manage API keys for Rig Control Plane'
    )
    
    subparsers = parser.add_subparsers(dest='command', help='Command to execute')
    
    # Generate command
    generate_parser = subparsers.add_parser('generate', help='Generate a new API key')
    generate_parser.add_argument(
        '--length',
        type=int,
        default=32,
        help='Length of random bytes (default: 32)'
    )
    generate_parser.add_argument(
        '--env-file',
        default='.env',
        help='Path to .env file (default: .env)'
    )
    generate_parser.add_argument(
        '--key-file',
        default='api_key.txt',
        help='Path to save API key (default: api_key.txt)'
    )
    
    # Verify command
    verify_parser = subparsers.add_parser('verify', help='Verify an API key')
    verify_parser.add_argument(
        'api_key',
        nargs='?',
        help='API key to verify (reads from stdin if not provided)'
    )
    verify_parser.add_argument(
        '--env-file',
        default='.env',
        help='Path to .env file (default: .env)'
    )
    
    # Hash command
    hash_parser = subparsers.add_parser('hash', help='Calculate hash of an API key')
    hash_parser.add_argument(
        'api_key',
        nargs='?',
        help='API key to hash (reads from stdin if not provided)'
    )
    
    args = parser.parse_args()
    
    if args.command is None:
        parser.print_help()
        sys.exit(1)
    
    if args.command == 'generate':
        print("🔑 Generating API Key for Rig Control Plane")
        print()
        
        # Generate API key
        api_key = generate_api_key(args.length)
        print(f"Generated API Key: {api_key}")
        print()
        
        # Calculate hash
        api_key_hash = hash_api_key(api_key)
        print(f"API Key Hash: {api_key_hash}")
        print()
        
        # Save to files
        save_api_key(api_key, api_key_hash, args.env_file, args.key_file)
        
    elif args.command == 'verify':
        # Get API key
        if args.api_key:
            api_key = args.api_key
        else:
            print("Enter API key to verify:")
            api_key = sys.stdin.readline().strip()
        
        # Load stored hash
        stored_hash = load_api_key_hash(args.env_file)
        if stored_hash is None:
            print(f"❌ No API key hash found in {args.env_file}")
            sys.exit(1)
        
        # Verify
        if verify_api_key(api_key, stored_hash):
            print("✅ API key is valid")
            sys.exit(0)
        else:
            print("❌ API key is invalid")
            sys.exit(1)
    
    elif args.command == 'hash':
        # Get API key
        if args.api_key:
            api_key = args.api_key
        else:
            print("Enter API key to hash:")
            api_key = sys.stdin.readline().strip()
        
        # Calculate and print hash
        api_key_hash = hash_api_key(api_key)
        print(f"API Key Hash: {api_key_hash}")

if __name__ == '__main__':
    main()
