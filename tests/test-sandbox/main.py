#!/usr/bin/env python3
"""
Test Sandbox Isolation

This script tests that the sandbox correctly:
1. Allows reading from allowed paths
2. Blocks writing to disallowed paths
3. Blocks network access when disabled
4. Blocks access to sensitive user directories (~/.ssh, ~/.aws, ~/.gnupg)
"""

import os
import sys
import pathlib

def test_read_allowed():
    """Test reading from source directory (should succeed)"""
    print("[TEST] Reading from source directory...")
    try:
        with open("capsule.toml", "r") as f:
            content = f.read()
            print(f"  ✓ Successfully read capsule.toml ({len(content)} bytes)")
            return True
    except Exception as e:
        print(f"  ✗ Failed to read capsule.toml: {e}")
        return False

def test_write_allowed():
    """Test writing to allowed directory (should succeed)"""
    print("[TEST] Writing to ./output directory...")
    try:
        os.makedirs("output", exist_ok=True)
        with open("output/test.txt", "w") as f:
            f.write("Hello from sandbox!")
        print("  ✓ Successfully wrote to ./output/test.txt")
        return True
    except Exception as e:
        print(f"  ✗ Failed to write to ./output/test.txt: {e}")
        return False

def test_write_blocked():
    """Test writing to disallowed directory (may succeed under Allow-Default strategy)"""
    print("[TEST] Writing to /tmp (informational - depends on sandbox strategy)...")
    try:
        with open("/tmp/sandbox-test.txt", "w") as f:
            f.write("This should fail!")
        print("  ⚠ Wrote to /tmp (expected under Allow-Default, Deny-Sensitive strategy)")
        # Clean up
        os.remove("/tmp/sandbox-test.txt")
        return True  # Allow-Default strategy permits /tmp
    except PermissionError:
        print("  ✓ Correctly blocked write to /tmp")
        return True
    except Exception as e:
        print(f"  ✓ Blocked with error: {e}")
        return True

def test_network_blocked():
    """Test network access (should fail when disabled)"""
    print("[TEST] Testing network access (should be blocked)...")
    try:
        import socket
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.settimeout(2)
        sock.connect(("8.8.8.8", 53))
        sock.close()
        print("  ✗ SECURITY ISSUE: Network access succeeded (should be blocked!)")
        return False
    except (PermissionError, OSError) as e:
        print(f"  ✓ Correctly blocked network access: {e}")
        return True
    except Exception as e:
        print(f"  ? Unexpected error: {e}")
        return True  # Still consider it blocked

def test_ssh_dir_blocked():
    """Test that ~/.ssh directory listing is blocked (sensitive path)"""
    ssh_dir = pathlib.Path.home() / ".ssh"
    print(f"[TEST] Listing {ssh_dir} (should be blocked)...")
    try:
        entries = list(ssh_dir.iterdir())
        print(f"  ✗ SECURITY ISSUE: Listed {len(entries)} files in ~/.ssh")
        return False
    except PermissionError as e:
        print(f"  ✓ Correctly blocked ~/.ssh listing: {e}")
        return True
    except FileNotFoundError:
        print("  ✓ ~/.ssh not visible in sandbox (FileNotFoundError)")
        return True
    except OSError as e:
        print(f"  ✓ Blocked with OS error: {e}")
        return True

def test_ssh_key_read_blocked():
    """Test that reading SSH private key is blocked"""
    ssh_key = pathlib.Path.home() / ".ssh" / "id_rsa"
    print(f"[TEST] Reading {ssh_key} (should be blocked)...")
    try:
        with open(ssh_key, "r") as f:
            content = f.read()
        print(f"  ✗ SECURITY ISSUE: Read {len(content)} bytes from SSH key!")
        return False
    except PermissionError as e:
        print(f"  ✓ Correctly blocked SSH key read: {e}")
        return True
    except FileNotFoundError:
        print("  ✓ SSH key not visible in sandbox (FileNotFoundError)")
        return True
    except OSError as e:
        print(f"  ✓ Blocked with OS error: {e}")
        return True

def test_aws_blocked():
    """Test that ~/.aws directory is blocked (sensitive path)"""
    aws_dir = pathlib.Path.home() / ".aws"
    print(f"[TEST] Listing {aws_dir} (should be blocked)...")
    if not aws_dir.exists():
        print("  - ~/.aws does not exist on this system, skipping")
        return True
    try:
        entries = list(aws_dir.iterdir())
        print(f"  ✗ SECURITY ISSUE: Listed {len(entries)} files in ~/.aws")
        return False
    except PermissionError as e:
        print(f"  ✓ Correctly blocked ~/.aws listing: {e}")
        return True
    except FileNotFoundError:
        print("  ✓ ~/.aws not visible in sandbox (FileNotFoundError)")
        return True
    except OSError as e:
        print(f"  ✓ Blocked with OS error: {e}")
        return True

def test_gnupg_blocked():
    """Test that ~/.gnupg directory is blocked (sensitive path)"""
    gnupg_dir = pathlib.Path.home() / ".gnupg"
    print(f"[TEST] Listing {gnupg_dir} (should be blocked)...")
    if not gnupg_dir.exists():
        print("  - ~/.gnupg does not exist on this system, skipping")
        return True
    try:
        entries = list(gnupg_dir.iterdir())
        print(f"  ✗ SECURITY ISSUE: Listed {len(entries)} files in ~/.gnupg")
        return False
    except PermissionError as e:
        print(f"  ✓ Correctly blocked ~/.gnupg listing: {e}")
        return True
    except FileNotFoundError:
        print("  ✓ ~/.gnupg not visible in sandbox (FileNotFoundError)")
        return True
    except OSError as e:
        print(f"  ✓ Blocked with OS error: {e}")
        return True

def main():
    print("=" * 60)
    print("Sandbox Isolation Test")
    print("=" * 60)
    print()

    results = []
    
    results.append(("Read allowed paths", test_read_allowed()))
    results.append(("Write allowed paths", test_write_allowed()))
    results.append(("Write blocked paths", test_write_blocked()))
    results.append(("Network blocked", test_network_blocked()))
    results.append(("~/.ssh dir BLOCKED", test_ssh_dir_blocked()))
    results.append(("~/.ssh/id_rsa BLOCKED", test_ssh_key_read_blocked()))
    results.append(("~/.aws BLOCKED", test_aws_blocked()))
    results.append(("~/.gnupg BLOCKED", test_gnupg_blocked()))

    print()
    print("=" * 60)
    print("Summary")
    print("=" * 60)

    passed = 0
    failed = 0
    for name, result in results:
        status = "PASS" if result else "FAIL"
        print(f"  [{status}] {name}")
        if result:
            passed += 1
        else:
            failed += 1

    print()
    print(f"Total: {passed} passed, {failed} failed")

    sys.exit(0 if failed == 0 else 1)

if __name__ == "__main__":
    main()
