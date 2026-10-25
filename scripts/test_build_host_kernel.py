"""Exercise the native builder with a small source tree and a fake compiler."""
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parent / "build-host-kernel.sh"
CONFIG = r'''#!/usr/bin/env python3
import os, sys
from pathlib import Path
args = iter(sys.argv[1:])
assert next(args) == '--file'
config = Path(next(args))
with config.open('a') as out:
    for action in args:
        option = next(args)
        if action == '--enable' and option != os.environ.get('DROP_OPTION'):
            out.write(f'CONFIG_{option}=y\n')
        elif action == '--disable':
            out.write(f'# CONFIG_{option} is not set\n')
        elif action == '--set-str':
            out.write(f'CONFIG_{option}="{next(args)}"\n')
'''
MAKE = r'''#!/usr/bin/env python3
import os, sys
from pathlib import Path
args = sys.argv[1:]
objects = Path(next(a[2:] for a in args if a.startswith('O=')))
with Path(os.environ['MAKE_LOG']).open('a') as log:
    log.write(' '.join(args) + '\n')
if args[-1] == 'defconfig':
    (objects / '.config').write_text('')
elif args[-1] in ('vmlinux', 'Image'):
    if os.environ.get('FAIL_BUILD'):
        sys.exit(17)
    target = objects / ('vmlinux' if args[-1] == 'vmlinux' else 'arch/arm64/boot/Image')
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(b'compiled ' + args[-1].encode())
'''


class NativeKernelBuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.tools = self.root / 'bin'
        self.tools.mkdir()
        for name in ('cc', 'flex', 'bison', 'bc', 'perl'):
            self.executable(name, '#!/bin/sh\nexit 0\n')
        self.executable('uname', '#!/bin/sh\nif [ "$1" = -s ]; then echo "${TEST_HOST_OS:-Linux}"; else echo "$TEST_HOST_ARCH"; fi\n')
        self.executable('make', MAKE)
        self.archive = self.root / 'linux-7.2.12.tar.xz'
        with tarfile.open(self.archive, 'w:xz') as archive:
            member = tarfile.TarInfo('linux-7.2.12/scripts/config')
            member.mode = 0o755
            data = CONFIG.encode()
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
        self.log = self.root / 'make.log'
        self.output = self.root / 'kernel'

    def executable(self, name, content):
        path = self.tools / name
        path.write_text(content)
        path.chmod(0o755)

    def build(self, architecture='x86_64', **overrides):
        env = dict(os.environ, PATH=f'{self.tools}{os.pathsep}{os.environ["PATH"]}',
                   TEST_HOST_ARCH=architecture, MAKE_LOG=str(self.log), FIRECRAB_KERNEL_BUILD_JOBS='2')
        env.update(overrides)
        return subprocess.run(['bash', str(SCRIPT), architecture, '7.2.12', str(self.archive),
                               str(self.root / 'work'), str(self.output)], env=env,
                              text=True, capture_output=True, check=False)

    def test_x86_build_has_required_builtin_drivers_and_preserves_config(self):
        result = self.build()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_bytes(), b'compiled vmlinux')
        config = self.output.with_name('kernel.config').read_text()
        for option in ('EXT4_FS', 'VIRTIO_BLK', 'VIRTIO_NET', 'VIRTIO_MMIO', 'VSOCKETS', 'VIRTIO_VSOCKETS'):
            self.assertIn(f'CONFIG_{option}=y', config)
        self.assertIn('# CONFIG_MODULES is not set', config)
        self.assertIn('ARCH=x86 -j2 vmlinux', self.log.read_text())

    def test_aarch64_build_produces_image_and_uses_4k_pages(self):
        result = self.build('aarch64')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_bytes(), b'compiled Image')
        self.assertIn('CONFIG_ARM64_4K_PAGES=y', self.output.with_name('kernel.config').read_text())
        self.assertIn('ARCH=arm64 -j2 Image', self.log.read_text())

    def test_missing_builtin_driver_stops_before_compilation(self):
        result = self.build(DROP_OPTION='VIRTIO_NET')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Missing built-in kernel option: VIRTIO_NET', result.stderr)
        self.assertFalse(self.output.exists())

    def test_failed_build_does_not_publish_a_kernel(self):
        result = self.build(FAIL_BUILD='1')
        self.assertEqual(result.returncode, 17)
        self.assertFalse(self.output.exists())

    def test_non_linux_host_is_rejected_before_extraction(self):
        result = self.build(TEST_HOST_OS='Darwin')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('require a Linux host', result.stderr)
        self.assertFalse((self.root / 'work').exists())

    def test_invalid_job_count_is_rejected(self):
        for value in ('0', '00', '-1', 'many'):
            with self.subTest(value=value):
                result = self.build(FIRECRAB_KERNEL_BUILD_JOBS=value)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('positive integer', result.stderr)
                self.assertFalse((self.root / 'work').exists())

    def test_relative_work_directory_becomes_absolute_for_make(self):
        env = dict(os.environ, PATH=f'{self.tools}{os.pathsep}{os.environ["PATH"]}',
                   TEST_HOST_ARCH='x86_64', MAKE_LOG=str(self.log))
        result = subprocess.run(['bash', str(SCRIPT), 'x86_64', '7.2.12', self.archive.name,
                                 'work', self.output.name], cwd=self.root, env=env,
                                text=True, capture_output=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f'O={self.root.resolve()}/work/objects', self.log.read_text())

    def test_foreign_host_architecture_is_rejected(self):
        result = self.build(TEST_HOST_ARCH='aarch64')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('matching host architecture', result.stderr)
        self.assertFalse((self.root / 'work').exists())

    def test_missing_compiler_tool_is_reported_before_extraction(self):
        # A deliberately isolated PATH still provides every other prerequisite.
        for name in ('bash', 'tar', 'xz', 'install'):
            target = shutil.which(name)
            if target:
                (self.tools / name).symlink_to(target)
        (self.tools / 'flex').unlink()
        env = dict(os.environ, PATH=str(self.tools), TEST_HOST_ARCH='x86_64')
        result = subprocess.run(['/bin/bash', str(SCRIPT), 'x86_64', '7.2.12', str(self.archive),
                                 str(self.root / 'work'), str(self.output)], env=env,
                                text=True, capture_output=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Missing kernel build tool: flex', result.stderr)
        self.assertFalse((self.root / 'work').exists())


if __name__ == '__main__':
    unittest.main()
