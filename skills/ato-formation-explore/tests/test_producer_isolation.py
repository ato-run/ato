"""Public-package/authority checks. Native agents and authentication never run."""

import importlib.util
import json
from pathlib import Path
import secrets
import subprocess
import sys
import tempfile
import unittest


SKILL = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "formation_producer_isolation", SKILL / "scripts/producer-isolation-preflight.py")
HELPER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HELPER)


@unittest.skipUnless(sys.platform == "darwin", "macOS package has Mach-O binary pins")
class ProducerIsolationTests(unittest.TestCase):
    def setUp(self):
        temporary = SKILL.parents[1] / ".tmp"
        temporary.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="producer-isolation-test-", dir=temporary)
        self.root = Path(self.temporary.name)
        self.source = self.root / "public-source"
        for relative in ["SKILL.md", "agents/openai.yaml", "references/protocol.md",
                         "scripts/install.py", "scripts/producer-isolation-preflight.py",
                         "scripts/producer-isolation-probe.c"]:
            path = self.source / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("public fixture\n")
        self.output = self.root / "package"
        self.selected_binary = Path("/usr/bin/true")

    def tearDown(self):
        self.temporary.cleanup()

    def prepare(self, codex_version="0.46.0", **network):
        return HELPER.prepare(self.output, "producer-isolation-unit", "a" * 40,
                              self.selected_binary, self.selected_binary, codex_version,
                              self.selected_binary, "2.1.288", source=self.source, **network)

    def test_fresh_copy_shared_links_and_old_codex_stays_ineligible(self):
        result = self.prepare()
        self.assertFalse(result["native_acceptance_ready"])
        self.assertFalse(result["codex_version_eligible"])
        self.assertFalse(result["native_agent_or_auth_invoked"])
        self.assertEqual(result["mcp_tools"], ["status", "next", "submit", "cancel"])
        for entry in [".agents", ".claude"]:
            link = self.output / "public/project" / entry / "skills/ato-formation-explore"
            self.assertEqual(link.resolve(), self.output / "public/skill")
            self.assertNotEqual(link.resolve(), self.source)
        self.assertEqual(HELPER.verify(self.output)["public_package_sha256"],
                         result["public_package_sha256"])

    def test_version_floor_does_not_mark_native_acceptance_complete(self):
        result = self.prepare("0.100.0")
        self.assertTrue(result["codex_version_eligible"])
        self.assertFalse(result["native_acceptance_ready"])
        self.assertIn("native_auth_ipc_without_tool_access", result["gates"])

    def test_existing_output_is_preserved_and_not_reinitialized(self):
        self.prepare()
        before = (self.output / "manifest.json").read_bytes()
        with self.assertRaisesRegex(HELPER.Rejected, "fresh_output_required"):
            self.prepare()
        self.assertTrue(before == (self.output / "manifest.json").read_bytes())

    def test_unknown_source_secret_and_old_measurement_are_not_copied(self):
        for name in ["auth.json", ".env", "old-measurement.json"]:
            (self.source / name).write_text(secrets.token_hex(32))
        result = self.prepare()
        files = {item["path"] for item in result["files"]}
        self.assertFalse(any(name in files for name in ["auth.json", ".env", "old-measurement.json"]))

    def test_source_symlink_to_private_value_is_rejected(self):
        path = self.source / "references/protocol.md"
        path.unlink()
        private = self.root / "private-canary"
        private.write_text(secrets.token_hex(32))
        path.symlink_to(private)
        with self.assertRaisesRegex(HELPER.Rejected, "public_package_symlink"):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_source_file_bounds_are_enforced_before_publication(self):
        (self.source / "SKILL.md").write_bytes(b"x" * (256 * 1024 + 1))
        with self.assertRaisesRegex(HELPER.Rejected, "public_package_file_bounds"):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_package_tampering_is_rejected_before_os_execution(self):
        self.prepare()
        path = self.output / "public/skill/SKILL.md"
        path.chmod(0o600)
        path.write_text("modified public instructions")
        with self.assertRaisesRegex(HELPER.Rejected, "public_package_changed"):
            HELPER.verify(self.output)
        self.assertFalse((self.output / "probe").exists())

    def test_extra_public_private_file_is_rejected(self):
        self.prepare()
        (self.output / "public/unexpected-credential").write_text(secrets.token_hex(32))
        with self.assertRaisesRegex(HELPER.Rejected, "public_layout_changed"):
            HELPER.verify(self.output)

    def test_package_symlink_and_scratch_redirect_are_rejected(self):
        self.prepare()
        extra = self.output / "public/skill/escape"
        extra.symlink_to(self.root / "missing-private-file")
        with self.assertRaisesRegex(HELPER.Rejected, "public_package_symlink"):
            HELPER.verify(self.output)
        extra.unlink()
        scratch = self.output / "scratch"
        scratch.rmdir()
        scratch.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(HELPER.Rejected, "package_directory_changed"):
            HELPER.verify(self.output)

    def test_fixed_mcp_and_profile_changes_are_rejected(self):
        self.prepare()
        policy = self.output / "codex.prepared.sb"
        policy.write_text("(version 1)\n(allow default)\n")
        with self.assertRaisesRegex(HELPER.Rejected, "profile_changed"):
            HELPER.verify(self.output)

    def test_fixed_mcp_connection_or_tool_expansion_is_rejected(self):
        self.prepare()
        path = self.output / "public/fixed-mcp.json"
        value = json.loads(path.read_text())
        value["tools"].append("host_exec")
        path.write_text(json.dumps(value))
        with self.assertRaisesRegex(HELPER.Rejected, "public_layout_changed"):
            HELPER.verify(self.output)

    def test_output_outside_workspace_tmp_is_rejected_without_writes(self):
        self.output = SKILL / "uncreated-output"
        with self.assertRaisesRegex(HELPER.Rejected, "workspace_tmp_output_required"):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_prepared_policy_has_exact_exec_no_home_or_network_allow(self):
        self.prepare()
        policy = (self.output / "claude-code.prepared.sb").read_text()
        self.assertIn("(deny default)", policy)
        self.assertIn('(allow file-read* (literal "/"))', policy)
        self.assertNotIn('(subpath "/")', policy)
        self.assertNotIn("(allow process-exec)", policy)
        self.assertNotIn("network-outbound", policy)
        self.assertNotIn("unix-socket", policy)
        self.assertNotIn("/Library/Keychains", policy)
        self.assertNotIn('(subpath "/Users")', policy)
        self.assertIn('(allow mach-lookup (global-name "com.apple.secd"))', policy)

    def test_error_output_cannot_echo_private_manifest_or_canary(self):
        self.prepare()
        value = secrets.token_hex(32)
        (self.output / "manifest.json").write_text(value)
        result = subprocess.run([sys.executable, str(SKILL / "scripts/producer-isolation-preflight.py"),
                                 "verify", "--package", str(self.output)], capture_output=True,
                                text=True, timeout=5)
        self.assertEqual(result.returncode, 1)
        self.assertFalse(value in result.stdout or value in result.stderr)
        self.assertEqual(json.loads(result.stderr), {"status": "rejected", "code": "preflight_failed"})

    def test_nonbinary_file_cannot_be_selected_as_native_auth_input(self):
        credential = self.root / "credential-file"
        credential.write_text(secrets.token_hex(32))
        credential.chmod(0o600)
        with self.assertRaisesRegex(HELPER.Rejected, "native_binary_required"):
            HELPER.binary(credential)

    def test_explicit_endpoints_grant_only_exact_tcp_with_no_dns_or_unix_grant(self):
        result = self.prepare(relay_endpoint="127.0.0.1:43123",
                              provider_endpoints=["8.8.8.8:443", "1.1.1.1:443"])
        policy = (self.output / "codex.prepared.sb").read_text()
        self.assertIn('(allow network-outbound (remote tcp "127.0.0.1:43123"))', policy)
        self.assertIn('(allow network-outbound (remote tcp "1.1.1.1:443"))', policy)
        self.assertIn('(allow network-outbound (remote tcp "8.8.8.8:443"))', policy)
        self.assertNotIn('(allow network-outbound)', policy)
        self.assertNotIn('network-inbound', policy)
        self.assertNotIn('system-socket', policy)
        self.assertNotIn('unix-socket', policy)
        self.assertNotIn('mDNSResponder', policy)
        self.assertNotIn('remote udp', policy)
        self.assertFalse(result['network_policy']['dns_allowed'])
        self.assertFalse(result['network_policy']['provider_identity_verified'])
        self.assertFalse(result['native_acceptance_ready'])
        self.assertTrue(HELPER.verify(self.output)['network_policy'] == result['network_policy'])
        fixed = json.loads((self.output / 'public/fixed-mcp.json').read_text())
        self.assertTrue(fixed['arguments'] == ['--relay', str(self.output / 'public/session-relay.json')])
        self.assertFalse(fixed['descriptor_present'])
        self.assertFalse(fixed['descriptor_binding_checked'])
        self.assertFalse((self.output / 'public/session-relay.json').exists())

    def test_wildcard_hostname_ipv6_url_range_control_and_zero_ports_are_rejected(self):
        for value in ['*:443', 'api.example.com:443', 'localhost:1234', '[::1]:1234',
                      'https://127.0.0.1:443', '127.0.0.1:1-65535', '127.0.0.1:0',
                      '127.0.0.1:65536', '127.0.0.1:0123', '127.0.0.1:443\n(allow default)']:
            with self.assertRaises(HELPER.Rejected):
                self.prepare(relay_endpoint=value)
            self.assertFalse(self.output.exists())

    def test_relay_must_match_broker_numeric_127_0_0_1(self):
        for value in ['127.0.0.2:1234', '8.8.8.8:443', '0.0.0.0:1234']:
            with self.assertRaisesRegex(HELPER.Rejected, 'relay_loopback_required'):
                self.prepare(relay_endpoint=value)
            self.assertFalse(self.output.exists())

    def test_provider_cannot_expand_to_private_metadata_multicast_or_non_tls_port(self):
        for value in ['127.0.0.1:443', '10.0.0.1:443', '169.254.169.254:443',
                      '192.0.2.1:443', '224.0.0.1:443', '0.0.0.0:443', '8.8.8.8:80']:
            with self.assertRaisesRegex(HELPER.Rejected, 'provider_public_ipv4_tls_port_required'):
                self.prepare(provider_endpoints=[value])
            self.assertFalse(self.output.exists())

    def test_provider_duplicates_and_overlong_allowlist_are_rejected(self):
        for values in [['8.8.8.8:443'] * 2,
                       ['8.8.8.8:443', '1.1.1.1:443', '9.9.9.9:443', '8.8.4.4:443', '1.0.0.1:443']]:
            with self.assertRaisesRegex(HELPER.Rejected, 'provider_endpoint_bounds'):
                self.prepare(provider_endpoints=values)
            self.assertFalse(self.output.exists())

    def test_network_claim_tampering_and_rehashed_broad_policy_are_rejected(self):
        self.prepare(relay_endpoint='127.0.0.1:43123')
        path = self.output / 'manifest.json'
        saved = json.loads(path.read_text())
        changed = json.loads(path.read_text())
        changed['network_policy']['provider_identity_verified'] = True
        path.write_text(json.dumps(changed))
        with self.assertRaisesRegex(HELPER.Rejected, 'network_policy_changed'):
            HELPER.verify(self.output)
        path.write_text(json.dumps(saved))
        policy = self.output / 'codex.prepared.sb'
        policy.write_text('(version 1)\n(allow default)\n')
        saved['profiles']['codex'] = HELPER.digest(policy)
        path.write_text(json.dumps(saved))
        with self.assertRaisesRegex(HELPER.Rejected, 'profile_policy_mismatch'):
            HELPER.verify(self.output)

    def test_invalid_endpoint_error_does_not_echo_private_argument(self):
        canary = secrets.token_hex(32)
        arguments = [sys.executable, str(SKILL / 'scripts/producer-isolation-preflight.py'),
                     'prepare', '--output', str(self.output), '--measurement-id', 'unit-network-private',
                     '--execution-pin', 'a' * 40, '--mcp-binary', str(self.selected_binary),
                     '--codex-binary', str(self.selected_binary), '--codex-version', '0.46.0',
                     '--claude-binary', str(self.selected_binary), '--claude-version', '2.1.288',
                     '--provider-endpoint', canary]
        result = subprocess.run(arguments, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 1)
        self.assertFalse(canary in result.stdout or canary in result.stderr)
        self.assertTrue(json.loads(result.stderr)['code'] == 'numeric_tcp_endpoint_required')
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
