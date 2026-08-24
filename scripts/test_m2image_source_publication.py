#!/usr/bin/env python3
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "m2image_source_publication", ROOT / "m2image_source_publication.py"
)
assert spec and spec.loader
sourcepub = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sourcepub)


def source_map(distribution, packages):
    return {
        "schemaVersion": 1,
        "image": {
            "alias": f"{distribution}-image",
            "version": "1",
            "distribution": distribution,
            "architecture": "x86_64",
        },
        "packages": packages,
    }


def package(name, version, source, license_text=None):
    return {
        "binaryPackage": name,
        "binaryVersion": version,
        "architecture": "x86_64",
        "declaredLicense": license_text,
        "source": source,
    }


def gpg_pubkey():
    return {
        "binaryPackage": "gpg-pubkey",
        "binaryVersion": "0:350d275d-627e00a1",
        "architecture": "x86_64",
        "declaredLicense": "pubkey",
        "sourceDisposition": "rpm-key-metadata",
    }


class SourcePublicationTests(unittest.TestCase):
    def test_alpine_deduplicates_subpackages_by_exact_source_identity(self):
        src = {
            "type": "alpine-aports",
            "sourcePackage": "busybox",
            "sourceVersion": "1.37.0-r31",
            "repositoryCommit": "a" * 40,
        }
        plan = sourcepub.publication_plan(
            source_map(
                "alpine",
                [
                    package("busybox", "1.37.0-r31", src, "GPL-2.0-only"),
                    package("busybox-binsh", "1.37.0-r31", src, "GPL-2.0-only"),
                ],
            )
        )
        self.assertEqual(plan["packageCount"], 2)
        self.assertEqual(plan["sourceBackedPackageCount"], 2)
        self.assertEqual(plan["nonSourcePackageCount"], 0)
        self.assertEqual(plan["sourceCount"], 1)
        self.assertEqual(len(plan["sources"][0]["binaryPackages"]), 2)
        self.assertEqual(plan["coveragePolicy"], "all-installed-packages")

    def test_ubuntu_requires_exact_source_version(self):
        with self.assertRaisesRegex(ValueError, "exact sourceVersion"):
            sourcepub.publication_plan(
                source_map(
                    "ubuntu",
                    [
                        package(
                            "linux-image-virtual",
                            "7.0.0-30.30",
                            {
                                "type": "ubuntu-source-package",
                                "sourcePackage": "linux-meta",
                                "sourceVersion": "",
                            },
                        )
                    ],
                )
            )

    def test_rocky_requires_real_source_rpm_identity(self):
        with self.assertRaisesRegex(ValueError, "not an SRPM"):
            sourcepub.publication_plan(
                source_map(
                    "rocky",
                    [
                        package(
                            "kernel-core",
                            "5.14.0-570.26.1.el9_6",
                            {
                                "type": "rocky-source-rpm",
                                "sourceArtifact": "kernel-core",
                                "sourceVersion": "5.14.0-570.26.1.el9_6",
                            },
                        )
                    ],
                )
            )

    def test_rocky_gpg_pubkey_is_covered_without_fake_source_unit(self):
        src = {
            "type": "rocky-source-rpm",
            "sourceArtifact": "bash-5.1.8-9.el9.src.rpm",
            "sourceVersion": "0:5.1.8-9.el9",
        }
        plan = sourcepub.publication_plan(
            source_map(
                "rocky",
                [package("bash", "0:5.1.8-9.el9", src, "GPLv3+"), gpg_pubkey()],
            )
        )
        self.assertEqual(plan["packageCount"], 2)
        self.assertEqual(plan["sourceBackedPackageCount"], 1)
        self.assertEqual(plan["nonSourcePackageCount"], 1)
        self.assertEqual(plan["sourceCount"], 1)
        self.assertEqual(plan["nonSourcePackages"][0]["name"], "gpg-pubkey")

    def test_non_source_disposition_cannot_exempt_real_package(self):
        record = gpg_pubkey()
        record["binaryPackage"] = "bash"
        with self.assertRaisesRegex(ValueError, "unsupported non-source disposition"):
            sourcepub.publication_plan(source_map("rocky", [record]))

    def test_plan_rejects_duplicate_binary_records(self):
        src = {
            "type": "ubuntu-source-package",
            "sourcePackage": "bash",
            "sourceVersion": "5.2.21-2ubuntu4",
        }
        record = package("bash", "5.2.21-2ubuntu4", src)
        with self.assertRaisesRegex(ValueError, "duplicate binary package"):
            sourcepub.publication_plan(source_map("ubuntu", [record, dict(record)]))

    def test_index_requires_material_for_every_source_unit(self):
        plan = sourcepub.publication_plan(
            source_map(
                "ubuntu",
                [
                    package(
                        "bash",
                        "5.2.21-2ubuntu4",
                        {
                            "type": "ubuntu-source-package",
                            "sourcePackage": "bash",
                            "sourceVersion": "5.2.21-2ubuntu4",
                        },
                    )
                ],
            )
        )
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir) / "sources"
            root.mkdir()
            with self.assertRaisesRegex(ValueError, "source directory is missing"):
                sourcepub.source_index(plan, root)

    def test_index_hashes_source_files_and_covers_all_binaries(self):
        src = {
            "type": "alpine-aports",
            "sourcePackage": "busybox",
            "sourceVersion": "1.37.0-r31",
            "repositoryCommit": "b" * 40,
        }
        plan = sourcepub.publication_plan(
            source_map(
                "alpine",
                [
                    package("busybox", "1.37.0-r31", src, "GPL-2.0-only"),
                    package("busybox-binsh", "1.37.0-r31", src, "GPL-2.0-only"),
                ],
            )
        )
        with tempfile.TemporaryDirectory() as tmpdir:
            bundle = Path(tmpdir) / "bundle"
            source_root = bundle / "sources"
            unit_dir = source_root / plan["sources"][0]["sourceId"]
            unit_dir.mkdir(parents=True)
            (unit_dir / "APKBUILD").write_text("pkgname=busybox\n", encoding="utf-8")
            (unit_dir / "busybox.tar.bz2").write_bytes(b"source")
            index = sourcepub.source_index(plan, source_root)
            self.assertEqual(index["packageCount"], 2)
            self.assertEqual(index["sourceCount"], 1)
            self.assertEqual(index["fileCount"], 2)
            for item in index["sources"][0]["files"]:
                self.assertEqual(len(item["sha256"]), 64)
                self.assertTrue(item["path"].startswith("sources/"))

    def test_index_counts_explicit_non_source_coverage(self):
        src = {
            "type": "rocky-source-rpm",
            "sourceArtifact": "bash-5.1.8-9.el9.src.rpm",
            "sourceVersion": "0:5.1.8-9.el9",
        }
        plan = sourcepub.publication_plan(
            source_map(
                "rocky",
                [package("bash", "0:5.1.8-9.el9", src), gpg_pubkey()],
            )
        )
        with tempfile.TemporaryDirectory() as tmpdir:
            bundle = Path(tmpdir) / "bundle"
            source_root = bundle / "sources"
            unit_dir = source_root / plan["sources"][0]["sourceId"]
            unit_dir.mkdir(parents=True)
            (unit_dir / "bash.src.rpm").write_bytes(b"srpm")
            index = sourcepub.source_index(plan, source_root)
            self.assertEqual(index["packageCount"], 2)
            self.assertEqual(index["sourceBackedPackageCount"], 1)
            self.assertEqual(index["nonSourcePackageCount"], 1)

    def test_plan_json_is_deterministic_across_input_order(self):
        a = package(
            "bash",
            "5.2",
            {
                "type": "ubuntu-source-package",
                "sourcePackage": "bash",
                "sourceVersion": "5.2",
            },
        )
        b = package(
            "coreutils",
            "9.5",
            {
                "type": "ubuntu-source-package",
                "sourcePackage": "coreutils",
                "sourceVersion": "9.5",
            },
        )
        first = sourcepub.publication_plan(source_map("ubuntu", [a, b]))
        second = sourcepub.publication_plan(source_map("ubuntu", [b, a]))
        self.assertEqual(
            json.dumps(first, sort_keys=True, separators=(",", ":")),
            json.dumps(second, sort_keys=True, separators=(",", ":")),
        )


if __name__ == "__main__":
    unittest.main()
