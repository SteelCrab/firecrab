import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import release_compliance as rc


def package(
    pid,
    name,
    version="1.0.0",
    *,
    license="MIT",
    kinds=None,
    manifest_path=None,
):
    return {
        "id": pid,
        "name": name,
        "version": version,
        "license": license,
        "license_file": None,
        "source": f"registry+https://example/{name}",
        "repository": None,
        "manifest_path": manifest_path or f"/missing/{name}/Cargo.toml",
        "targets": [{"kind": kinds or ["lib"]}],
    }


class ReleaseComplianceTests(unittest.TestCase):
    def cargo_fixture(self, root: Path):
        api_dir = root / "api"
        dep_dir = root / "runtime"
        dev_dir = root / "dev"
        proc_dir = root / "macro"
        for directory in (api_dir, dep_dir, dev_dir, proc_dir):
            directory.mkdir()
            (directory / "Cargo.toml").write_text(
                "[package]\nname='x'\nversion='0.1.0'\n", encoding="utf-8"
            )
        (dep_dir / "LICENSE-MIT").write_text("MIT text\n", encoding="utf-8")
        return {
            "packages": [
                package(
                    "api",
                    "firecrab-api",
                    manifest_path=str(api_dir / "Cargo.toml"),
                ),
                package(
                    "runtime",
                    "runtime-dep",
                    manifest_path=str(dep_dir / "Cargo.toml"),
                ),
                package(
                    "dev",
                    "dev-dep",
                    manifest_path=str(dev_dir / "Cargo.toml"),
                ),
                package(
                    "macro",
                    "macro-dep",
                    kinds=["proc-macro"],
                    manifest_path=str(proc_dir / "Cargo.toml"),
                ),
            ],
            "workspace_members": ["api"],
            "resolve": {
                "nodes": [
                    {
                        "id": "api",
                        "deps": [
                            {
                                "pkg": "runtime",
                                "dep_kinds": [{"kind": None, "target": None}],
                            },
                            {
                                "pkg": "dev",
                                "dep_kinds": [{"kind": "dev", "target": None}],
                            },
                            {
                                "pkg": "macro",
                                "dep_kinds": [{"kind": None, "target": None}],
                            },
                        ],
                    },
                    {"id": "runtime", "deps": []},
                    {"id": "dev", "deps": []},
                    {"id": "macro", "deps": []},
                ]
            },
        }

    def test_cargo_sets_separate_runtime_from_dev_and_proc_macro(self):
        with tempfile.TemporaryDirectory() as tmp:
            runtime, build = rc.cargo_sets(self.cargo_fixture(Path(tmp)))
        self.assertEqual(runtime, {"runtime"})
        self.assertEqual(build, {"dev", "macro"})

    def test_npm_sets_use_lockfile_dev_flag(self):
        lock = {
            "packages": {
                "": {"name": "app"},
                "node_modules/react": {
                    "version": "19.0.0",
                    "license": "MIT",
                },
                "node_modules/vite": {
                    "version": "8.0.0",
                    "license": "MIT",
                    "dev": True,
                },
            }
        }
        runtime, build = rc.npm_sets(lock)
        self.assertEqual([item["name"] for item in runtime], ["react"])
        self.assertEqual([item["name"] for item in build], ["vite"])

    def test_license_policy_rejects_gpl_only_but_accepts_dual_and_lgpl(self):
        self.assertFalse(rc.license_allowed("GPL-2.0-only"))
        self.assertFalse(rc.license_allowed("AGPL-3.0-only"))
        self.assertTrue(rc.license_allowed("MIT OR GPL-2.0-only"))
        self.assertTrue(rc.license_allowed("LGPL-2.1-only"))
        self.assertTrue(rc.license_allowed("Apache-2.0"))
        self.assertFalse(rc.license_allowed(None))

    def test_license_policy_fails_closed_on_unreviewed_declarations(self):
        for expression in (
            "Proprietary",
            "GPLv2",
            "LicenseRef-file:COPYING",
            "SEE LICENSE IN LICENSE",
            "Unknown-License-1.0",
        ):
            with self.subTest(expression=expression):
                self.assertFalse(rc.license_allowed(expression))
        self.assertFalse(rc.license_allowed("MIT AND GPL-2.0-only"))
        self.assertTrue(rc.license_allowed("MIT AND BSD-3-Clause"))
        self.assertTrue(rc.license_allowed("MIT/Apache-2.0"))
        self.assertTrue(
            rc.license_allowed("Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT")
        )

    def test_cargo_records_keep_packaged_license_text(self):
        with tempfile.TemporaryDirectory() as tmp:
            metadata = self.cargo_fixture(Path(tmp))
            runtime, build = rc.cargo_sets(metadata)
            runtime_records, _ = rc.cargo_records([metadata], runtime, build)
        self.assertEqual(runtime_records[0]["name"], "runtime-dep")
        self.assertEqual(runtime_records[0]["notices"][0]["name"], "LICENSE-MIT")
        self.assertIn("MIT text", runtime_records[0]["notices"][0]["text"])

    def test_render_notices_distinguishes_runtime_and_build_only(self):
        runtime = [
            {
                "ecosystem": "npm",
                "name": "react",
                "version": "19",
                "license": "MIT",
                "source": None,
                "notices": [],
            }
        ]
        build = [
            {
                "ecosystem": "npm",
                "name": "vite",
                "version": "8",
                "license": "MIT",
                "source": None,
                "notices": [],
            }
        ]
        text = rc.render_notices(runtime, build)
        self.assertIn("Runtime dependencies: 1", text)
        self.assertIn("Build/test-only dependencies: 1", text)
        self.assertIn("npm :: react 19", text)
        self.assertIn("Build/test-only inventory", text)
        self.assertIn("npm :: vite 8 :: MIT", text)

    def test_main_writes_inventory_and_fails_on_incompatible_runtime_license(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            metadata = self.cargo_fixture(root)
            for item in metadata["packages"]:
                if item["id"] == "runtime":
                    item["license"] = "GPL-2.0-only"
            cargo = root / "cargo.json"
            lock = root / "package-lock.json"
            frontend = root / "frontend"
            frontend.mkdir()
            cargo.write_text(json.dumps(metadata), encoding="utf-8")
            lock.write_text(
                json.dumps({"packages": {"": {"name": "app"}}}),
                encoding="utf-8",
            )
            notices = root / "out" / "THIRD_PARTY_NOTICES.txt"
            inventory = root / "out" / "release-license-inventory.json"
            code = rc.main(
                [
                    "--cargo-metadata",
                    str(cargo),
                    "--frontend-lock",
                    str(lock),
                    "--frontend-root",
                    str(frontend),
                    "--notices-out",
                    str(notices),
                    "--inventory-out",
                    str(inventory),
                    "--deny-incompatible",
                ]
            )
            self.assertEqual(code, 1)
            self.assertTrue(notices.is_file())
            data = json.loads(inventory.read_text(encoding="utf-8"))
            self.assertEqual(len(data["runtime"]), 1)

    def test_multi_target_union_keeps_target_only_runtime_packages(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)

            def target_metadata(label: str, dep_id: str):
                api_dir = root / f"api-{label}"
                dep_dir = root / dep_id
                api_dir.mkdir()
                dep_dir.mkdir()
                (api_dir / "Cargo.toml").write_text(
                    "[package]\nname='firecrab-api'\nversion='0.1.0'\n",
                    encoding="utf-8",
                )
                (dep_dir / "Cargo.toml").write_text(
                    f"[package]\nname='{dep_id}'\nversion='1.0.0'\n",
                    encoding="utf-8",
                )
                return {
                    "packages": [
                        package(
                            "api",
                            "firecrab-api",
                            manifest_path=str(api_dir / "Cargo.toml"),
                        ),
                        package(
                            dep_id,
                            dep_id,
                            manifest_path=str(dep_dir / "Cargo.toml"),
                        ),
                    ],
                    "workspace_members": ["api"],
                    "resolve": {
                        "nodes": [
                            {
                                "id": "api",
                                "deps": [
                                    {
                                        "pkg": dep_id,
                                        "dep_kinds": [{"kind": None, "target": None}],
                                    }
                                ],
                            },
                            {"id": dep_id, "deps": []},
                        ]
                    },
                }

            x86 = target_metadata("x86", "x86-runtime")
            arm = target_metadata("arm", "arm-runtime")
            runtime, build = rc.merge_cargo_sets([x86, arm])
            runtime_records, build_records = rc.cargo_records(
                [x86, arm], runtime, build
            )

        self.assertEqual(runtime, {"x86-runtime", "arm-runtime"})
        self.assertEqual(build, set())
        self.assertEqual(
            {item["name"] for item in runtime_records},
            {"x86-runtime", "arm-runtime"},
        )
        self.assertEqual(build_records, [])


if __name__ == "__main__":
    unittest.main()
