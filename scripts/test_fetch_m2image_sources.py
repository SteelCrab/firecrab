#!/usr/bin/env python3
import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "fetch_m2image_sources", ROOT / "fetch_m2image_sources.py"
)
assert spec and spec.loader
fetcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetcher)


class AlpineDistfilesRecoveryTests(unittest.TestCase):
    def test_literal_sha512sums_extracts_exact_filename(self):
        payload = b"archived source bytes"
        digest = hashlib.sha512(payload).hexdigest()
        with tempfile.TemporaryDirectory() as tmpdir:
            apkbuild = Path(tmpdir) / "APKBUILD"
            apkbuild.write_text(
                'pkgname=iproute2\n'
                'source="https://kernel.example/iproute2-v7.0.0.tar.xz"\n'
                'sha512sums="\n'
                f'{digest}  iproute2-v7.0.0.tar.xz\n'
                '"\n',
                encoding="utf-8",
            )
            self.assertEqual(
                fetcher._literal_sha512sums(apkbuild),
                {"iproute2-v7.0.0.tar.xz": digest},
            )

    def test_dynamic_or_unverified_checksum_syntax_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            apkbuild = Path(tmpdir) / "APKBUILD"
            apkbuild.write_text(
                'sha512sums="\nSKIP  source.tar.xz\n"\n', encoding="utf-8"
            )
            self.assertEqual(fetcher._literal_sha512sums(apkbuild), {})

    def test_alpine_branch_and_builder_image_come_from_release_series(self):
        image = {"version": "3.24.1"}
        self.assertEqual(fetcher._alpine_distfiles_branch(image), "v3.24")
        self.assertEqual(fetcher._alpine_abuild_image(image), "alpine:3.24")
        with self.assertRaisesRegex(ValueError, "cannot derive Alpine release series"):
            fetcher._alpine_distfiles_branch({"version": "edge"})
        with self.assertRaisesRegex(ValueError, "cannot derive Alpine release series"):
            fetcher._alpine_abuild_image({"version": "edge"})

    def test_abuild_fetch_uses_image_series_in_docker_tag(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            work = root / "recipe"
            dist = root / "distfiles"
            work.mkdir()
            dist.mkdir()
            completed = mock.Mock(returncode=0)
            with mock.patch.object(fetcher.subprocess, "run", return_value=completed) as run:
                result = fetcher._alpine_abuild_fetch(
                    work, dist, {"version": "3.25.2"}
                )
            self.assertEqual(result, 0)
            command = run.call_args.args[0]
            self.assertIn("alpine:3.25", command)
            self.assertNotIn("alpine:3.24", command)

    def test_recovery_accepts_only_checksum_matching_archive(self):
        payload = b"iproute2 exact source bytes"
        digest = hashlib.sha512(payload).hexdigest()
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            recipe = root / "recipe"
            distfiles = root / "distfiles"
            recipe.mkdir()
            distfiles.mkdir()
            (recipe / "APKBUILD").write_text(
                'sha512sums="\n'
                f'{digest}  iproute2-v7.0.0.tar.xz\n'
                '"\n',
                encoding="utf-8",
            )

            def fake_fetch(url, output):
                self.assertEqual(
                    url,
                    "https://distfiles.alpinelinux.org/distfiles/v3.24/"
                    "iproute2-v7.0.0.tar.xz",
                )
                output.write_bytes(payload)
                return True

            with mock.patch.object(fetcher, "fetch_url", side_effect=fake_fetch):
                recovered = fetcher._recover_alpine_distfiles(
                    recipe, distfiles, {"version": "3.24.1"}
                )

            self.assertEqual(len(recovered), 1)
            self.assertEqual(
                (distfiles / "iproute2-v7.0.0.tar.xz").read_bytes(), payload
            )

    def test_recovery_rejects_checksum_mismatch_and_removes_file(self):
        expected = hashlib.sha512(b"expected").hexdigest()
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            recipe = root / "recipe"
            distfiles = root / "distfiles"
            recipe.mkdir()
            distfiles.mkdir()
            (recipe / "APKBUILD").write_text(
                'sha512sums="\n'
                f'{expected}  source.tar.xz\n'
                '"\n',
                encoding="utf-8",
            )

            def fake_fetch(_url, output):
                output.write_bytes(b"wrong")
                return True

            with mock.patch.object(fetcher, "fetch_url", side_effect=fake_fetch):
                with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                    fetcher._recover_alpine_distfiles(
                        recipe, distfiles, {"version": "3.24.1"}
                    )
            self.assertFalse((distfiles / "source.tar.xz").exists())

    def test_local_recipe_files_are_never_replaced_from_distfiles(self):
        payload = b"local patch"
        digest = hashlib.sha512(payload).hexdigest()
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)
            recipe = root / "recipe"
            distfiles = root / "distfiles"
            recipe.mkdir()
            distfiles.mkdir()
            (recipe / "fix.patch").write_bytes(payload)
            (recipe / "APKBUILD").write_text(
                'sha512sums="\n'
                f'{digest}  fix.patch\n'
                '"\n',
                encoding="utf-8",
            )
            with mock.patch.object(fetcher, "fetch_url") as mocked:
                recovered = fetcher._recover_alpine_distfiles(
                    recipe, distfiles, {"version": "3.24.1"}
                )
            self.assertEqual(recovered, [])
            mocked.assert_not_called()


class RockySourceFetchTests(unittest.TestCase):
    def test_fetch_rejects_tampered_artifact_path_before_network(self):
        unit = {
            "source": {
                "sourceArtifact": "../../evil.src.rpm",
                "sourceVersion": "1-1.el9",
            }
        }
        with tempfile.TemporaryDirectory() as tmpdir:
            destination = Path(tmpdir)
            with (
                mock.patch.object(fetcher, "require_tool"),
                mock.patch.object(fetcher, "fetch_url") as mocked_fetch,
            ):
                with self.assertRaisesRegex(ValueError, "bare filename"):
                    fetcher.fetch_rocky(
                        unit,
                        {"distribution": "rocky", "version": "9.8"},
                        destination,
                    )
            mocked_fetch.assert_not_called()
            self.assertFalse((destination.parent / "evil.src.rpm").exists())


class BatchedSourceFetchTests(unittest.TestCase):
    def test_ubuntu_full_fetch_refreshes_source_index_once(self):
        units = [
            {
                "sourceId": "one",
                "source": {
                    "type": "ubuntu-source-package",
                    "sourcePackage": "alpha",
                    "sourceVersion": "1.0-1",
                },
            },
            {
                "sourceId": "two",
                "source": {
                    "type": "ubuntu-source-package",
                    "sourcePackage": "beta",
                    "sourceVersion": "2.0-1",
                },
            },
        ]
        image = {"distribution": "ubuntu", "version": "26.04"}
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)

            def fake_fetch(unit, destination, _options):
                (destination / f"{unit['sourceId']}.dsc").write_text(
                    "source", encoding="utf-8"
                )

            with (
                mock.patch.object(fetcher, "require_tool"),
                mock.patch.object(fetcher, "run") as mocked_run,
                mock.patch.object(
                    fetcher, "_fetch_ubuntu_unit", side_effect=fake_fetch
                ) as mocked_fetch,
            ):
                fetcher._materialize_ubuntu(units, image, root)

            self.assertEqual(mocked_run.call_count, 1)
            self.assertIn("update", mocked_run.call_args.args[0])
            self.assertEqual(mocked_fetch.call_count, 2)
            self.assertTrue((root / "one" / "one.dsc").is_file())
            self.assertTrue((root / "two" / "two.dsc").is_file())

    def test_rocky_full_fetch_consumes_dynamic_plan_with_bounded_workers(self):
        units = [
            {
                "sourceId": f"source-{number}",
                "source": {
                    "type": "rocky-source-rpm",
                    "sourceArtifact": f"pkg-{number}.src.rpm",
                    "sourceVersion": f"{number}-1.el9",
                },
            }
            for number in range(5)
        ]
        image = {"distribution": "rocky", "version": "9.8"}
        with tempfile.TemporaryDirectory() as tmpdir:
            root = Path(tmpdir)

            def fake_fetch(unit, _image, destination):
                artifact = unit["source"]["sourceArtifact"]
                (destination / artifact).write_bytes(b"srpm")

            with (
                mock.patch.object(fetcher, "require_tool"),
                mock.patch.object(fetcher, "fetch_rocky", side_effect=fake_fetch) as mocked,
                mock.patch.object(
                    fetcher.concurrent.futures, "ThreadPoolExecutor", wraps=fetcher.concurrent.futures.ThreadPoolExecutor
                ) as executor,
            ):
                fetcher._materialize_rocky(units, image, root, workers=2)

            self.assertEqual(mocked.call_count, len(units))
            self.assertEqual(executor.call_args.kwargs["max_workers"], 2)
            for unit in units:
                self.assertTrue(
                    (root / unit["sourceId"] / unit["source"]["sourceArtifact"]).is_file()
                )

    def test_materialize_rejects_zero_workers(self):
        plan = {
            "schemaVersion": 1,
            "coveragePolicy": "all-installed-packages",
            "image": {"distribution": "rocky", "version": "9.8"},
            "sources": [{"sourceId": "x", "source": {}}],
        }
        with tempfile.TemporaryDirectory() as tmpdir:
            with self.assertRaisesRegex(ValueError, "workers must be at least 1"):
                fetcher.materialize(
                    plan,
                    Path(tmpdir) / "out",
                    Path(tmpdir) / "cache",
                    workers=0,
                )


if __name__ == "__main__":
    unittest.main()
