#!/usr/bin/env python3
from pathlib import Path
import re

ci = Path('.github/workflows/ci.yml')
text = ci.read_text()

def replace_once(old: str, new: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f'expected one match, got {count}: {old!r}')
    text = text.replace(old, new, 1)

replace_once(
    '(see below); that needs an OCI import, covered by the vm-boot jobs.',
    '(see below); that needs an M2Image package install, covered by vm-boot.',
)
replace_once(
    '            scripts/bake-install-sh.sh scripts/ci-prepare-install-payload.sh \\\n            scripts/verify-release-binary.sh \\\n',
    '            scripts/bake-install-sh.sh scripts/ci-prepare-install-payload.sh \\\n            scripts/ci-install-m2image.sh \\\n            scripts/verify-release-binary.sh \\\n',
)
replace_once(
    '# Keep in sync with firecrab-api/src/templates.rs default_specs().',
    '# Keep in sync with packaging/m2images.json and known_specs().',
)
replace_once(
    '        # Alpine is the M2Image catalog default; install.sh no longer\n'
    '        # preinstalls a guest image itself (see the next step).\n',
    '        # install.sh intentionally leaves guest images to MicroRegistry or\n'
    '        # MicroBoot; exercise the public MicroRegistry install path next.\n',
)

text, alias_count = re.subn(r'alpine-3\.24(?!\.1)', 'alpine-3.24.1', text)
if alias_count < 1:
    raise SystemExit('no retired alpine-3.24 aliases found')

hosted = re.compile(
    r'      - name: Import guest image\n.*?(?=      - name: Images on disk\n)',
    re.S,
)
hosted_repl = '''      - name: Install M2Image
        run: |
          chmod +x scripts/ci-install-m2image.sh
          scripts/ci-install-m2image.sh "${{ matrix.template }}"
          echo "BOOT_TEMPLATE=${{ matrix.template }}" >>"$GITHUB_ENV"

'''
text, count = hosted.subn(hosted_repl, text, count=1)
if count != 1:
    raise SystemExit(f'hosted import block replacements: {count}')

self_hosted = re.compile(
    r'      - name: Import guest image\n.*?(?=      - name: Boot M2 guest\n)',
    re.S,
)
self_repl = '''      - name: Install M2Image
        run: |
          chmod +x scripts/ci-install-m2image.sh
          scripts/ci-install-m2image.sh "${{ matrix.template }}"
          echo "BOOT_TEMPLATE=${{ matrix.template }}" >>"$GITHUB_ENV"

'''
text, count = self_hosted.subn(self_repl, text, count=1)
if count != 1:
    raise SystemExit(f'self-hosted import block replacements: {count}')

replace_once(
    '          sudo ls -l /var/lib/firecrab/images/rootfs/ || true\n'
    '          curl -fsS http://127.0.0.1:5523/api/vms\n',
    '          sudo ls -l /var/lib/firecrab/images/rootfs/ || true\n'
    '          curl -fsS http://127.0.0.1:5523/api/images\n'
    '          curl -fsS http://127.0.0.1:5523/api/vms\n',
)

if '      - name: Import guest image\n' in text:
    raise SystemExit('stale OCI import block remains')
if re.search(r'alpine-3\.24(?!\.1)', text):
    raise SystemExit('stale alpine-3.24 alias remains')
ci.write_text(text)

firewall = Path('firecrab-net-helper/src/firewall.rs')
fw = firewall.read_text()
occurrences = fw.count('priority dstnat')
if occurrences < 1:
    raise SystemExit('no symbolic dstnat priorities found')
fw = fw.replace('priority dstnat', 'priority -100')
if 'priority dstnat' in fw:
    raise SystemExit('symbolic dstnat priority remains')
firewall.write_text(fw)

print(f'updated {alias_count} Alpine aliases and {occurrences} nft priorities')
