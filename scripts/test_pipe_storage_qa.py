#!/usr/bin/env python3
"""Cleanup contracts for S3 QA. No AWS account or API is contacted."""
import argparse
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import URLError

source = Path(__file__).with_name("test-pipe-storage-aws.py")
spec = importlib.util.spec_from_file_location("s3_qa", source)
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


class StorageQaCleanup(unittest.TestCase):
    def ledger(self, directory):
        name = "firecrab-439-" + "a" * 20
        path = Path(directory) / "s3-resources.json"
        qa.write_json(path, {"issue": 439, "run": "a" * 20, "bucket": name, "role": name, "account": "123456789012", "cleaned": False})
        return path

    def test_cleanup_rejects_unrelated_resources_and_accounts(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.ledger(directory)
            record = json.loads(path.read_text())
            record["bucket"] = "production"
            qa.write_json(path, record)
            with patch.object(qa, "aws") as aws, self.assertRaises(ValueError):
                qa.cleanup(path)
            aws.assert_not_called()
            path = self.ledger(directory)
            with patch.object(qa, "aws", return_value={"Account": "999999999999"}) as aws, self.assertRaises(ValueError):
                qa.cleanup(path)
            self.assertEqual(aws.call_count, 1)

    def test_cleanup_preserves_unexpected_data_and_still_removes_the_test_role(self):
        calls = []
        def aws(service, operation, **_arguments):
            calls.append(operation)
            if operation == "get-caller-identity":
                return {"Account": "123456789012"}
            if operation == "list-objects-v2":
                return {"Contents": [{"Key": "unexpected/file"}]}
            return {}
        with tempfile.TemporaryDirectory() as directory:
            path = self.ledger(directory)
            with patch.object(qa, "aws", side_effect=aws), self.assertRaises(RuntimeError):
                qa.cleanup(path)
            self.assertNotIn("delete-bucket", calls)
            self.assertNotIn("delete-objects", calls)
            self.assertIn("delete-role", calls)
            self.assertFalse(json.loads(path.read_text())["cleaned"])

    def test_missing_image_stops_before_creating_cloud_resources(self):
        args = argparse.Namespace(api="http://127.0.0.1:15539", isolated_api=True)
        with patch.object(qa, "aws") as aws, self.assertRaisesRegex(ValueError, "Pipe Auth"):
            qa.run(args)
        aws.assert_not_called()

    def test_api_failures_cannot_skip_cloud_cleanup_or_persist_session_keys(self):
        calls = []
        def aws(service, operation, **_arguments):
            calls.append(operation)
            if operation == "get-caller-identity":
                return {"Account": "123456789012"}
            if operation == "create-role":
                return {"Role": {"Arn": "arn:aws:iam::123456789012:role/test"}}
            if operation == "create-user":
                return {"User": {"Arn": "arn:aws:iam::123456789012:user/test"}}
            if operation == "create-access-key":
                return {"AccessKey": {"AccessKeyId": "AKIA_TEST_BOOTSTRAP", "SecretAccessKey": "private-bootstrap-secret"}}
            if operation == "list-access-keys":
                return {"AccessKeyMetadata": [{"AccessKeyId": "AKIA_TEST_BOOTSTRAP"}]}
            if operation == "assume-role":
                return {"Credentials": {"AccessKeyId": "ASIA_TEST_KEY", "SecretAccessKey": "private-test-secret-" * 3, "SessionToken": "private-session-token-" * 4}}
            return {}
        class Connection(io.BytesIO):
            status = 201
        with tempfile.TemporaryDirectory() as directory:
            args = argparse.Namespace(api="http://127.0.0.1:15539", isolated_api=True, service_image="ghcr.io/qa/app:test", region="ap-northeast-2", output=Path(directory)/"report.json")
            with patch.object(qa, "aws", side_effect=aws), patch.object(qa, "urlopen", side_effect=[Connection(b'{"id":"connection"}'), URLError("API stopped"), URLError("API stopped")]), self.assertRaises(RuntimeError):
                qa.run(args)
            self.assertIn("delete-bucket", calls)
            self.assertIn("delete-role", calls)
            self.assertIn("delete-access-key", calls)
            self.assertIn("delete-user", calls)
            self.assertTrue(json.loads((Path(directory)/"s3-resources.json").read_text())["cleaned"])
            for path in Path(directory).iterdir():
                self.assertNotIn("private-test-secret", path.read_text())
                self.assertNotIn("private-session-token", path.read_text())
                self.assertNotIn("private-bootstrap-secret", path.read_text())
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
