import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location("mail_probe", Path(__file__).with_name("mailbox.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class RecordedMailbox:
    def __init__(self, message):
        self.message = message
        self.matches = b"7"

    def uid(self, operation, *args):
        if operation == "search":
            return "OK", [self.matches]
        return "OK", [(b"7 (BODY[]", self.message.as_bytes())]


class PersistenceEvidence(unittest.TestCase):
    def test_exactly_one_matching_message_with_identical_body(self):
        message = probe.marker()
        evidence = {"message_id": str(message["Message-ID"]), "body_sha256": probe.body_hash(message)}
        client = RecordedMailbox(message)
        probe.verify(client, evidence)
        for matches in (b"", b"3 7"):
            client.matches = matches
            with self.assertRaisesRegex(ValueError, "exactly one"):
                probe.verify(client, evidence)
        client.matches = b"7"
        message.set_content("different")
        with self.assertRaisesRegex(ValueError, "differs"):
            probe.verify(client, evidence)


if __name__ == "__main__":
    unittest.main()
