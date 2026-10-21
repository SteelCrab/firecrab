"""Test the exact guest file-restoration script embedded in the signed helper."""
import hashlib
import os
from pathlib import Path
import tempfile
import types
import unittest

SOURCE = Path(__file__).resolve().parents[1] / 'micromanager-macos/Sources/FirecrabMicroManager/OSFileRestore.swift'
SCRIPT = SOURCE.read_text().split('#"""\n', 1)[1].rsplit('"""#', 1)[0]
GUEST = types.ModuleType('os_file_restore')
exec(compile(SCRIPT, str(SOURCE), 'exec'), GUEST.__dict__)


class OSFileRestoreTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / 'target'
        self.reference = Path(self.temporary.name) / 'reference'
        for root in [self.root, self.reference]:
            (root / 'var/lib/dpkg/info').mkdir(parents=True)
            (root / 'usr/lib/example').mkdir(parents=True)
            (root / 'var/lib/dpkg/status').write_text(
                'Package: example\nStatus: install ok installed\nArchitecture: arm64\nVersion: 1\n\n')
        self.binary = '/usr/lib/example/library.so.1'
        self.link = '/usr/lib/example/library.so'
        payload = b'checked Debian package contents'
        (self.reference / self.binary.lstrip('/')).write_bytes(payload)
        (self.reference / self.link.lstrip('/')).symlink_to('library.so.1')
        digest = hashlib.md5(payload).hexdigest()
        (self.root / 'var/lib/dpkg/info/example:arm64.list').write_text(self.binary + '\n' + self.link + '\n')
        (self.root / 'var/lib/dpkg/info/example:arm64.md5sums').write_text(digest + '  ' + self.binary.lstrip('/') + '\n')

    def restore(self):
        return GUEST.restore_missing(self.root, self.reference)

    def test_missing_binary_and_symlink_are_restored_and_repeat_is_noop(self):
        self.assertEqual(self.restore(), 2)
        self.assertEqual((self.root / self.binary.lstrip('/')).read_bytes(), b'checked Debian package contents')
        self.assertEqual(os.readlink(self.root / self.link.lstrip('/')), 'library.so.1')
        self.assertEqual(self.restore(), 0)
        self.assertEqual((self.reference / self.binary.lstrip('/')).read_bytes(), b'checked Debian package contents')

    def test_existing_files_and_configuration_are_preserved(self):
        existing = self.root / self.binary.lstrip('/')
        existing.write_bytes(b'local override')
        config = self.root / 'etc/example.conf'
        config.parent.mkdir()
        config.write_text('local configuration')
        manifest = self.root / 'var/lib/dpkg/info/example:arm64.list'
        manifest.write_text(manifest.read_text() + '/etc/example.conf\n')
        self.assertEqual(self.restore(), 1)
        self.assertEqual(existing.read_bytes(), b'local override')
        self.assertEqual(config.read_text(), 'local configuration')

    def test_reference_version_mismatch_fails_before_replacing_missing_file(self):
        path = self.reference / 'var/lib/dpkg/status'
        path.write_text(path.read_text().replace('Version: 1', 'Version: 2'))
        with self.assertRaisesRegex(RuntimeError, 'version does not match'):
            self.restore()
        self.assertFalse((self.root / self.binary.lstrip('/')).exists())

    def test_reference_checksum_mismatch_and_missing_source_fail(self):
        path = self.reference / self.binary.lstrip('/')
        path.write_bytes(b'wrong file')
        with self.assertRaisesRegex(RuntimeError, 'checksum'):
            self.restore()
        path.unlink()
        with self.assertRaisesRegex(RuntimeError, 'cannot restore'):
            self.restore()

    def test_missing_parent_directory_is_created_without_recursive_copy(self):
        (self.root / 'usr/lib/example').rmdir()
        (self.reference / 'usr/lib/example/unlisted').write_bytes(b'do not copy')
        self.assertEqual(self.restore(), 2)
        self.assertFalse((self.root / 'usr/lib/example/unlisted').exists())

    def test_absolute_reference_symlinks_stay_within_the_reference(self):
        (self.reference / 'lib').symlink_to('/usr/lib')
        self.assertEqual(GUEST.rooted_path(self.reference, '/lib/example/library.so.1'),
                         self.reference / 'usr/lib/example/library.so.1')

    def test_destination_symlink_into_application_data_is_rejected(self):
        (self.root / 'usr/lib/example').rmdir()
        (self.root / 'var/lib/application').mkdir()
        (self.root / 'usr/lib/example').symlink_to('/var/lib/application')
        with self.assertRaisesRegex(RuntimeError, 'leaves OS binaries'):
            self.restore()
        self.assertEqual(list((self.root / 'var/lib/application').iterdir()), [])

    def test_missing_package_manifests_use_only_the_same_version_cache(self):
        for suffix in ['.list', '.md5sums']:
            source = self.root / ('var/lib/dpkg/info/example:arm64' + suffix)
            (self.reference / 'var/lib/dpkg/info' / source.name).write_bytes(source.read_bytes())
            source.unlink()
        self.assertEqual(self.restore(), 4)
        self.assertTrue((self.root / 'var/lib/dpkg/info/example:arm64.md5sums').is_file())
        self.assertEqual(self.restore(), 0)

    def test_checksum_matched_orphan_restores_the_installed_version(self):
        manifest = self.root / 'var/lib/dpkg/info/example:arm64.list'
        manifest.write_text(self.binary + '\n')
        lost = self.root / 'lost+found'
        lost.mkdir()
        source = self.reference / self.binary.lstrip('/')
        (lost / '#1234').write_bytes(source.read_bytes())
        source.write_bytes(b'different cached version')
        status = self.reference / 'var/lib/dpkg/status'
        status.write_text(status.read_text().replace('Version: 1', 'Version: 2'))
        self.assertEqual(self.restore(), 1)
        self.assertEqual((self.root / self.binary.lstrip('/')).read_bytes(), (lost / '#1234').read_bytes())
        self.assertTrue((lost / '#1234').exists())

    def test_package_mode_is_restored_after_owner_changes(self):
        source = self.reference / self.binary.lstrip('/')
        source.chmod(0o4755)
        self.assertEqual(self.restore(), 2)
        self.assertEqual((self.root / self.binary.lstrip('/')).stat().st_mode & 0o7777, 0o4755)

    def test_parent_traversal_is_rejected(self):
        (self.root / 'var/lib/dpkg/info/example:arm64.list').write_text('/usr/lib/../../etc/unsafe\n')
        with self.assertRaisesRegex(RuntimeError, 'invalid package path'):
            self.restore()


if __name__ == '__main__':
    unittest.main()
