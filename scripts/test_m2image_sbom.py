#!/usr/bin/env python3
import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

MODULE_PATH = Path(__file__).with_name("m2image_sbom.py")
spec = importlib.util.spec_from_file_location("m2image_sbom", MODULE_PATH)
assert spec and spec.loader
sbom = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sbom)


class M2ImageSbomTests(unittest.TestCase):
    def test_parse_alpine_installed_database(self):
        packages = sbom.parse_alpine(
            "P:busybox\n"
            "V:1.37.0-r18\n"
            "A:x86_64\n"
            "L:GPL-2.0-only\n"
            "o:busybox\n"
            "\n"
            "P:linux-virt\n"
            "V:6.15.4-r0\n"
            "A:x86_64\n"
            "L:GPL-2.0-only\n"
            "o:linux-lts\n"
        )
        self.assertEqual([p["name"] for p in packages], ["busybox", "linux-virt"])
        self.assertEqual(packages[1]["version"], "6.15.4-r0")
        self.assertEqual(packages[1]["source"], "linux-lts")

    def test_parse_dpkg_ignores_non_installed_entries(self):
        packages = sbom.parse_dpkg(
            "Package: linux-image-6.17.0-10-generic\n"
            "Status: install ok installed\n"
            "Architecture: amd64\n"
            "Version: 6.17.0-10.10\n"
            "Source: linux-signed (6.17.0-10.10)\n"
            "\n"
            "Package: removed-package\n"
            "Status: deinstall ok config-files\n"
            "Architecture: amd64\n"
            "Version: 1.0\n"
        )
        self.assertEqual(len(packages), 1)
        self.assertEqual(packages[0]["name"], "linux-image-6.17.0-10-generic")
        self.assertEqual(packages[0]["source"], "linux-signed")

    def test_parse_rpm_tsv_retains_license_and_source_rpm(self):
        packages = sbom.parse_rpm_tsv(
            "kernel-core\t0:5.14.0-570.26.1.el9_6\tx86_64\tGPLv2\tkernel-5.14.0-570.26.1.el9_6.src.rpm\n"
        )
        self.assertEqual(packages[0]["license"], "GPLv2")
        self.assertTrue(packages[0]["source"].endswith(".src.rpm"))

    def test_rpm_gpg_pubkey_is_explicit_non_source_metadata(self):
        packages = sbom.parse_rpm_tsv(
            "gpg-pubkey\t0:350d275d-627e00a1\t(none)\tpubkey\t(none)\n"
        )
        self.assertEqual(packages[0]["source"], "")
        self.assertEqual(packages[0]["source_disposition"], "rpm-key-metadata")
        document = sbom.make_spdx(
            distribution="rocky",
            image_alias="rocky-9.8",
            image_version="9.8",
            architecture="x86_64",
            packages=packages,
        )
        self.assertIn("source-disposition=rpm-key-metadata", document["packages"][1]["comment"])
        self.assertNotIn("source-package=", document["packages"][1]["comment"])

    def test_rpm_non_key_package_without_source_rpm_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "has no SOURCERPM"):
            sbom.parse_rpm_tsv("local-rpm\t0:3.0-1\tx86_64\tMIT\t(none)\n")

    def test_rpm_tsv_rejects_wrong_field_count(self):
        with self.assertRaisesRegex(ValueError, "expected 5 fields"):
            sbom.parse_rpm_tsv("kernel-core\t1.2\tx86_64\n")

    def test_spdx_is_sorted_and_records_actual_kernel_version(self):
        packages = [
            {"name": "zlib", "version": "1.3", "arch": "amd64", "license": "", "source": "zlib"},
            {
                "name": "linux-image-6.17.0-10-generic",
                "version": "6.17.0-10.10",
                "arch": "amd64",
                "license": "",
                "source": "linux-signed",
            },
        ]
        with mock.patch.dict(os.environ, {"SOURCE_DATE_EPOCH": "0"}):
            document = sbom.make_spdx(
                distribution="ubuntu",
                image_alias="ubuntu-26.04",
                image_version="26.04",
                architecture="x86_64",
                packages=packages,
            )
        self.assertEqual(document["creationInfo"]["created"], "1970-01-01T00:00:00Z")
        self.assertEqual([p["name"] for p in document["packages"][1:]], ["linux-image-6.17.0-10-generic", "zlib"])
        self.assertIn("linux-image-6.17.0-10-generic@6.17.0-10.10", document["annotations"][0]["comment"])
        self.assertEqual(document["relationships"][0]["relationshipType"], "DESCRIBES")
        self.assertTrue(all(r["relationshipType"] == "CONTAINS" for r in document["relationships"][1:]))

    def test_spdx_namespace_is_content_stable(self):
        packages_a = [
            {"name": "busybox", "version": "1.37.0-r18", "arch": "aarch64", "license": "GPL-2.0-only", "source": "busybox"},
            {"name": "linux-virt", "version": "6.15.4-r0", "arch": "aarch64", "license": "GPL-2.0-only", "source": "linux-lts"},
        ]
        packages_b = list(reversed(packages_a))
        with mock.patch.dict(os.environ, {"SOURCE_DATE_EPOCH": "123"}):
            a = sbom.make_spdx(
                distribution="alpine",
                image_alias="alpine-3.24.1",
                image_version="3.24.1",
                architecture="aarch64",
                packages=packages_a,
            )
            b = sbom.make_spdx(
                distribution="alpine",
                image_alias="alpine-3.24.1",
                image_version="3.24.1",
                architecture="aarch64",
                packages=packages_b,
            )
        self.assertEqual(a["documentNamespace"], b["documentNamespace"])
        self.assertEqual(json.dumps(a, sort_keys=True), json.dumps(b, sort_keys=True))
        purls = [p["externalRefs"][0]["referenceLocator"] for p in a["packages"][1:]]
        self.assertTrue(all(p.startswith("pkg:apk/alpine/") for p in purls))

    def test_duplicate_package_records_are_rejected(self):
        pkg = {"name": "busybox", "version": "1", "arch": "x86_64", "license": "", "source": ""}
        with self.assertRaisesRegex(ValueError, "duplicate"):
            sbom.make_spdx(
                distribution="alpine",
                image_alias="alpine",
                image_version="1",
                architecture="x86_64",
                packages=[pkg, dict(pkg)],
            )

    def test_cli_writes_spdx_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            db = Path(tmp) / "installed"
            out = Path(tmp) / "sbom.spdx.json"
            db.write_text("P:busybox\nV:1.37.0-r18\nA:x86_64\nL:GPL-2.0-only\no:busybox\n", encoding="utf-8")
            with mock.patch.dict(os.environ, {"SOURCE_DATE_EPOCH": "0"}):
                rc = sbom.main(
                    [
                        "--format", "alpine",
                        "--distribution", "alpine",
                        "--image-alias", "alpine-3.24.1",
                        "--image-version", "3.24.1",
                        "--architecture", "x86_64",
                        "--package-db", str(db),
                        "--output", str(out),
                    ]
                )
            self.assertEqual(rc, 0)
            parsed = json.loads(out.read_text(encoding="utf-8"))
            self.assertEqual(parsed["spdxVersion"], "SPDX-2.3")
            self.assertEqual(parsed["packages"][1]["name"], "busybox")

    def test_source_identity_falls_back_to_binary_name_for_apk_and_dpkg(self):
        alpine = sbom.parse_alpine("P:local-apk\nV:1-r0\nA:x86_64\nL:MIT\n")
        dpkg = sbom.parse_dpkg(
            "Package: same-source-deb\n"
            "Status: install ok installed\n"
            "Architecture: amd64\n"
            "Version: 2.0-1\n"
        )
        self.assertEqual(alpine[0]["source"], "local-apk")
        self.assertEqual(dpkg[0]["source"], "same-source-deb")

    def test_parsed_source_identity_or_disposition_is_never_implicit(self):
        packages = []
        packages += sbom.parse_alpine("P:a\nV:1\n")
        packages += sbom.parse_dpkg(
            "Package: d\nStatus: install ok installed\nVersion: 1\nArchitecture: all\n"
        )
        packages += sbom.parse_rpm_tsv("gpg-pubkey\t0:1-1\t(none)\tpubkey\t(none)\n")
        self.assertTrue(
            all(pkg.get("source") or pkg.get("source_disposition") for pkg in packages)
        )

    def test_source_provenance_preserves_alpine_commit_and_dpkg_source_version(self):
        alpine = sbom.parse_alpine(
            "P:busybox\nV:1.37.0-r31\nA:x86_64\nL:GPL-2.0-only\n"
            "o:busybox\nc:0123456789abcdef0123456789abcdef01234567\n"
        )[0]
        deb = sbom.parse_dpkg(
            "Package: linux-image-virtual\nStatus: install ok installed\n"
            "Architecture: amd64\nVersion: 7.0.0-30.30\n"
            "Source: linux-meta (7.0.0.30.30)\n"
        )[0]
        self.assertEqual(alpine["source_version"], "1.37.0-r31")
        self.assertEqual(alpine["source_commit"], "0123456789abcdef0123456789abcdef01234567")
        self.assertEqual(deb["source"], "linux-meta")
        self.assertEqual(deb["source_version"], "7.0.0.30.30")

    def test_spdx_emits_source_version_and_commit_evidence(self):
        packages = sbom.parse_alpine(
            "P:busybox\nV:1.37.0-r31\nA:x86_64\nL:GPL-2.0-only\n"
            "o:busybox\nc:0123456789abcdef0123456789abcdef01234567\n"
        )
        document = sbom.make_spdx(
            distribution="alpine",
            image_alias="alpine-3.24.1",
            image_version="3.24.1",
            architecture="x86_64",
            packages=packages,
        )
        comment = document["packages"][1]["comment"]
        self.assertIn("source-version=1.37.0-r31", comment)
        self.assertIn("source-commit=0123456789abcdef0123456789abcdef01234567", comment)


if __name__ == "__main__":
    unittest.main()
