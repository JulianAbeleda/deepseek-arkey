import unittest

from scripts.check_commit_msg import validate_message_text


class CommitMessageTests(unittest.TestCase):
    def test_accepts_allowed_prefixes(self):
        self.assertTrue(validate_message_text("[cli] Default to workspace agent mode")[0])
        self.assertTrue(validate_message_text("[provider] Update model IDs")[0])
        self.assertTrue(validate_message_text("[repo] NFC - Rename local helper")[0])

    def test_rejects_unprefixed_subject(self):
        ok, detail = validate_message_text("Default to workspace agent mode")

        self.assertFalse(ok)
        self.assertIn("must start with [subsystem] prefix", detail)

    def test_rejects_unknown_prefix(self):
        ok, detail = validate_message_text("[agent] Default to workspace agent mode")

        self.assertFalse(ok)
        self.assertIn("unknown commit prefix [agent]", detail)

    def test_rejects_bad_nfc_format(self):
        ok, detail = validate_message_text("[repo] NFC Rename local helper")

        self.assertFalse(ok)
        self.assertIn("NFC commits must use format", detail)


if __name__ == "__main__":
    unittest.main()
