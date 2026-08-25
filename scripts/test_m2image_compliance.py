#!/usr/bin/env python3
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("m2image_compliance.py")
spec = importlib.util.spec_from_file_location("m2image_compliance", MODULE_PATH)
assert spec and spec.loader
compliance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compliance)


def spdx(packages, distribution="alpine"):
    return {
        "spdxVersion": "SPDX-2.3",
        "packages": [
            {
                "name": f"{distribution}-image",
                "versionInfo": "1",
                "comment": f"distribution={distribution}; architecture=x86_64",
            },
            *packages,
        ],
    }


class M2ImageComplianceTests(unittest.TestCase):
    def test_source_map_requires_source_identity(self):
        doc = spdx([{"name": "busybox", "versionInfo": "1.37.0-r31"}])
        with self.assertRaisesRegex(ValueError, "missing source-package identity"):
            compliance.source_map(doc)

    def test_source_map_records_exact_alpine_commit(self):
        doc = spdx(
            [
                {
                    "name": "busybox",
                    "versionInfo": "1.37.0-r31",
                    "comment": (
                        "package-manager-license=GPL-2.0-only; source-package=busybox; "
                        "source-version=1.37.0-r31; "
                        "source-commit=0123456789abcdef0123456789abcdef01234567"
                    ),
                }
            ]
        )
        mapped = compliance.source_map(doc)["packages"][0]
        self.assertEqual(mapped["declaredLicense"], "GPL-2.0-only")
        self.assertEqual(mapped["source"]["sourcePackage"], "busybox")
        self.assertEqual(mapped["source"]["sourceVersion"], "1.37.0-r31")
        self.assertEqual(
            mapped["source"]["repositoryCommit"],
            "0123456789abcdef0123456789abcdef01234567",
        )
        self.assertTrue(mapped["source"]["metadataUrl"].endswith("0123456789abcdef0123456789abcdef01234567"))

    def test_source_map_preserves_binary_package_architecture_from_purl(self):
        doc = spdx(
            [
                {
                    "name": "filesystem",
                    "versionInfo": "3.16-5.el9",
                    "externalRefs": [
                        {
                            "referenceCategory": "PACKAGE_MANAGER",
                            "referenceType": "purl",
                            "referenceLocator": "pkg:rpm/rocky/filesystem@3.16-5.el9?arch=noarch",
                        }
                    ],
                    "comment": "source-package=filesystem-3.16-5.el9.src.rpm",
                }
            ],
            distribution="rocky",
        )
        result = compliance.source_map(doc)
        self.assertEqual(result["image"]["architecture"], "x86_64")
        self.assertEqual(result["packages"][0]["architecture"], "noarch")

    def test_ubuntu_and_rocky_resolvers_preserve_source_evidence(self):
        ubuntu = {
            "spdxVersion": "SPDX-2.3",
            "packages": [
                {
                    "name": "ubuntu-26.04",
                    "versionInfo": "26.04",
                    "comment": "distribution=ubuntu; architecture=x86_64",
                },
                {
                    "name": "linux-image-virtual",
                    "versionInfo": "7.0.0-30.30",
                    "comment": "source-package=linux-meta; source-version=7.0.0.30.30",
                },
            ],
        }
        rocky = {
            "spdxVersion": "SPDX-2.3",
            "packages": [
                {
                    "name": "rocky-9.8",
                    "versionInfo": "9.8",
                    "comment": "distribution=rocky; architecture=x86_64",
                },
                {
                    "name": "kernel-core",
                    "versionInfo": "0:5.14.0-687.41.1.el9_8",
                    "comment": "source-package=kernel-5.14.0-687.41.1.el9_8.src.rpm",
                },
            ],
        }
        ubuntu_source = compliance.source_map(ubuntu)["packages"][0]["source"]
        rocky_source = compliance.source_map(rocky)["packages"][0]["source"]
        self.assertEqual(ubuntu_source["sourcePackage"], "linux-meta")
        self.assertEqual(ubuntu_source["sourceVersion"], "7.0.0.30.30")
        self.assertIn("launchpad.net/ubuntu/+source/linux-meta/", ubuntu_source["metadataUrl"])
        self.assertEqual(
            rocky_source["sourceArtifact"],
            "kernel-5.14.0-687.41.1.el9_8.src.rpm",
        )

    def test_rocky_gpg_pubkey_records_explicit_non_source_disposition(self):
        doc = spdx(
            [
                {
                    "name": "gpg-pubkey",
                    "versionInfo": "0:350d275d-627e00a1",
                    "comment": "source-disposition=rpm-key-metadata",
                }
            ],
            distribution="rocky",
        )
        mapped = compliance.source_map(doc)["packages"][0]
        self.assertEqual(mapped["sourceDisposition"], "rpm-key-metadata")
        self.assertNotIn("source", mapped)

    def test_non_source_disposition_is_narrowly_scoped(self):
        doc = spdx(
            [
                {
                    "name": "bash",
                    "versionInfo": "5.1",
                    "comment": "source-disposition=rpm-key-metadata",
                }
            ],
            distribution="rocky",
        )
        with self.assertRaisesRegex(ValueError, "unsupported source-disposition"):
            compliance.source_map(doc)

    def test_bundle_copies_guest_legal_material_and_canonical_gpl(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            tmp = Path(tmpdir)
            legal = tmp / "guest"
            (legal / "usr/share/licenses/busybox").mkdir(parents=True)
            (legal / "usr/share/licenses/busybox/COPYING").write_text("guest gpl\n")
            (legal / "usr/share/doc/curl").mkdir(parents=True)
            (legal / "usr/share/doc/curl/copyright").write_text("curl notice\n")
            (legal / "usr/share/doc/curl/changelog.gz").write_bytes(b"not legal evidence")
            gpl = tmp / "GPL-2.0-only.txt"
            gpl.write_text("canonical gpl\n")
            sbom_path = tmp / "sbom.json"
            sbom_path.write_text(
                json.dumps(
                    spdx(
                        [
                            {
                                "name": "busybox",
                                "versionInfo": "1.37.0-r31",
                                "comment": "package-manager-license=GPL-2.0-only; source-package=busybox",
                            }
                        ]
                    )
                )
            )
            out = tmp / "bundle"
            summary = compliance.build_bundle(
                spdx_path=sbom_path,
                legal_root=legal,
                gpl2_text=gpl,
                output_dir=out,
            )
            self.assertEqual(summary["packageCount"], 1)
            self.assertEqual(summary["guestLegalFileCount"], 2)
            self.assertTrue((out / "licenses/GPL-2.0-only.txt").is_file())
            self.assertTrue((out / "licenses/guest/usr/share/licenses/busybox/COPYING").is_file())
            self.assertTrue((out / "licenses/guest/usr/share/doc/curl/copyright").is_file())
            self.assertFalse((out / "licenses/guest/usr/share/doc/curl/changelog.gz").exists())
            index = json.loads((out / "licenses/index.json").read_text())
            self.assertEqual(len(index["files"]), 3)
            self.assertTrue(all(item["sha256"] for item in index["files"]))
            self.assertEqual(
                (out / "sbom.spdx.json").read_bytes(), sbom_path.read_bytes()
            )

    def test_bundle_is_deterministic_for_same_inputs(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            tmp = Path(tmpdir)
            legal = tmp / "guest"
            (legal / "usr/share/common-licenses").mkdir(parents=True)
            (legal / "usr/share/common-licenses/GPL-2").write_text("gpl common\n")
            gpl = tmp / "GPL.txt"
            gpl.write_text("canonical\n")
            sbom_path = tmp / "sbom.json"
            sbom_path.write_text(
                json.dumps(
                    spdx(
                        [
                            {
                                "name": "busybox",
                                "versionInfo": "1",
                                "comment": "source-package=busybox",
                            }
                        ]
                    ),
                    sort_keys=True,
                )
            )
            first = tmp / "first"
            second = tmp / "second"
            compliance.build_bundle(
                spdx_path=sbom_path, legal_root=legal, gpl2_text=gpl, output_dir=first
            )
            compliance.build_bundle(
                spdx_path=sbom_path, legal_root=legal, gpl2_text=gpl, output_dir=second
            )
            for rel in ("bundle.json", "source-map.json", "licenses/index.json", "README.txt"):
                self.assertEqual((first / rel).read_bytes(), (second / rel).read_bytes())


if __name__ == "__main__":
    unittest.main()
