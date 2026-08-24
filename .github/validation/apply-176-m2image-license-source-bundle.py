#!/usr/bin/env python3
from pathlib import Path


def replace_once(path, old, new, label):
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match, got {count}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


# Preserve exact source-package evidence exposed by each package DB.
replace_once(
    "scripts/m2image_sbom.py",
    '                    "source": fields.get("o") or fields["P"],\n',
    '                    "source": fields.get("o") or fields["P"],\n'
    '                    "source_version": fields["V"],\n'
    '                    "source_commit": fields.get("c", ""),\n',
    "Alpine source evidence",
)
replace_once(
    "scripts/m2image_sbom.py",
    '        source = fields.get("Source") or fields["Package"]\n'
    '        source = source.split(" ", 1)[0]\n'
    "        packages.append(\n",
    '        source_field = fields.get("Source", "")\n'
    '        source = fields["Package"]\n'
    '        source_version = fields["Version"]\n'
    "        if source_field:\n"
    '            match = re.fullmatch(r"([^\\s(]+)(?:\\s+\\(([^)]+)\\))?", source_field)\n'
    "            if not match:\n"
    '                raise ValueError(f"invalid dpkg Source field: {source_field!r}")\n'
    "            source = match.group(1)\n"
    "            source_version = match.group(2) or source_version\n"
    "        packages.append(\n",
    "dpkg source evidence parser",
)
replace_once(
    "scripts/m2image_sbom.py",
    '                "license": "",\n'
    '                "source": source,\n'
    "            }\n",
    '                "license": "",\n'
    '                "source": source,\n'
    '                "source_version": source_version,\n'
    '                "source_commit": "",\n'
    "            }\n",
    "dpkg source evidence record",
)
replace_once(
    "scripts/m2image_sbom.py",
    '                "license": license_text,\n'
    '                "source": name if source == "(none)" or not source else source,\n'
    "            }\n",
    '                "license": license_text,\n'
    '                "source": name if source == "(none)" or not source else source,\n'
    '                "source_version": version,\n'
    '                "source_commit": "",\n'
    "            }\n",
    "RPM source evidence record",
)
replace_once(
    "scripts/m2image_sbom.py",
    '        if pkg.get("source"):\n'
    '            comments.append(f"source-package={pkg[\'source\']}")\n'
    "        if comments:\n",
    '        if pkg.get("source"):\n'
    '            comments.append(f"source-package={pkg[\'source\']}")\n'
    '        if pkg.get("source_version"):\n'
    '            comments.append(f"source-version={pkg[\'source_version\']}")\n'
    '        if pkg.get("source_commit"):\n'
    '            comments.append(f"source-commit={pkg[\'source_commit\']}")\n'
    "        if comments:\n",
    "SPDX source evidence comments",
)

# Build the evidence bundle from the actual ext4 after every builder.
replace_once(
    "scripts/build-m2images.sh",
    '  python3 - "$sbom_output" "$alias" <<\'PY_VALIDATE\'\n'
    "import json, sys\n"
    "doc = json.load(open(sys.argv[1], encoding='utf-8'))\n"
    "assert doc.get('spdxVersion') == 'SPDX-2.3'\n"
    "assert doc.get('packages', [{}])[0].get('name') == sys.argv[2]\n"
    "PY_VALIDATE\n"
    "done\n",
    '  python3 - "$sbom_output" "$alias" <<\'PY_VALIDATE\'\n'
    "import json, sys\n"
    "doc = json.load(open(sys.argv[1], encoding='utf-8'))\n"
    "assert doc.get('spdxVersion') == 'SPDX-2.3'\n"
    "assert doc.get('packages', [{}])[0].get('name') == sys.argv[2]\n"
    "PY_VALIDATE\n"
    '  M2IMAGE_MANIFEST="$manifest" IMAGE_ROOT="${repo_dir}/images" \\\n'
    '    M2IMAGE_COMPLIANCE_DIR="$compliance_dir" \\\n'
    '    bash "${script_dir}/collect-m2image-compliance.sh" "$alias" "$architecture"\n'
    '  [ -s "${compliance_dir}/${alias}-${architecture}/bundle.json" ] \\\n'
    '    || fail "builder did not produce M2Image compliance bundle for ${alias}/${architecture}"\n'
    "done\n",
    "build compliance collection",
)

# Release archives require the complete evidence bundle, not only an SBOM.
replace_once(
    "scripts/package-m2images.sh",
    '  local sbom_source="${compliance_root}/${alias}-${architecture}.spdx.json"\n'
    '  [ -s "$sbom_source" ] || fail "missing M2Image SBOM: $sbom_source (build $alias first)"\n'
    '  python3 - "$sbom_source" "$alias" <<\'PY_SBOM\'\n'
    "import json, sys\n"
    "doc = json.load(open(sys.argv[1], encoding='utf-8'))\n"
    "assert doc.get('spdxVersion') == 'SPDX-2.3', 'M2Image SBOM is not SPDX 2.3'\n"
    "assert doc.get('packages', [{}])[0].get('name') == sys.argv[2], 'M2Image SBOM alias mismatch'\n"
    "PY_SBOM\n"
    '  mkdir -p "$staging/compliance"\n'
    '  cp -- "$sbom_source" "$staging/compliance/sbom.spdx.json"\n'
    '  files+=("compliance/sbom.spdx.json")\n',
    '  local compliance_source="${compliance_root}/${alias}-${architecture}"\n'
    '  [ -d "$compliance_source" ] \\\n'
    '    || fail "missing M2Image compliance bundle: $compliance_source (build $alias first)"\n'
    '  for required in bundle.json source-map.json sbom.spdx.json README.txt \\\n'
    "    licenses/index.json licenses/GPL-2.0-only.txt; do\n"
    '    [ -s "${compliance_source}/${required}" ] \\\n'
    '      || fail "missing M2Image compliance artifact: ${compliance_source}/${required}"\n'
    "  done\n"
    '  python3 - "${compliance_source}/sbom.spdx.json" "$alias" <<\'PY_SBOM\'\n'
    "import json, sys\n"
    "doc = json.load(open(sys.argv[1], encoding='utf-8'))\n"
    "assert doc.get('spdxVersion') == 'SPDX-2.3', 'M2Image SBOM is not SPDX 2.3'\n"
    "assert doc.get('packages', [{}])[0].get('name') == sys.argv[2], 'M2Image SBOM alias mismatch'\n"
    "PY_SBOM\n"
    '  mkdir -p "$staging/compliance"\n'
    '  cp -a -- "$compliance_source/." "$staging/compliance/"\n'
    '  files+=("compliance")\n',
    "package compliance bundle",
)

# Strengthen the synthetic archive contract to exercise debugfs collection.
p = Path("scripts/test-m2image-package-compliance.sh")
text = p.read_text(encoding="utf-8")
old = '      mkdir -p "$stage/etc"\n      printf \'synthetic\\n\' >"$stage/etc/os-release"\n'
new = (
    '      mkdir -p "$stage/etc" "$stage/usr/share/licenses/busybox" "$stage/usr/share/doc/curl"\n'
    '      printf \'synthetic\\n\' >"$stage/etc/os-release"\n'
    '      printf \'busybox license\\n\' >"$stage/usr/share/licenses/busybox/COPYING"\n'
    '      printf \'curl copyright\\n\' >"$stage/usr/share/doc/curl/copyright"\n'
)
if text.count(old) != 1:
    raise SystemExit("package contract root-stage anchor mismatch")
text = text.replace(old, new, 1)
marker = (
    'SOURCE_DATE_EPOCH=0 python3 "$root/scripts/m2image_sbom.py" \\\n'
    '  --format alpine --distribution alpine --image-alias "$alias" \\\n'
    '  --image-version 3.24.1 --architecture "$arch" \\\n'
    '  --package-db "$tmp/apk-installed" \\\n'
    '  --output "$image_root/compliance/${alias}-${arch}.spdx.json"\n\n'
)
if marker not in text:
    raise SystemExit("package contract SBOM marker missing")
text = text.replace(
    marker,
    marker
    + 'M2IMAGE_COMPLIANCE_DIR="$image_root/compliance" IMAGE_ROOT="$image_root" \\\n'
    + '  bash "$root/scripts/collect-m2image-compliance.sh" "$alias" "$arch"\n\n',
    1,
)
old = (
    'grep -qx \'compliance/sbom.spdx.json\' "$tmp/members"\n\n'
    'rm "$image_root/compliance/${alias}-${arch}.spdx.json"\n'
    'if IMAGE_ROOT="$image_root" OUT_DIR="$tmp/no-sbom" ZSTD_LEVEL=1 ZSTD_THREADS=1 \\\n'
)
new = (
    'grep -qx \'compliance/sbom.spdx.json\' "$tmp/members"\n'
    'grep -qx \'compliance/bundle.json\' "$tmp/members"\n'
    'grep -qx \'compliance/source-map.json\' "$tmp/members"\n'
    'grep -qx \'compliance/licenses/index.json\' "$tmp/members"\n'
    'grep -qx \'compliance/licenses/GPL-2.0-only.txt\' "$tmp/members"\n'
    'grep -qx \'compliance/licenses/guest/usr/share/licenses/busybox/COPYING\' "$tmp/members"\n'
    'grep -qx \'compliance/licenses/guest/usr/share/doc/curl/copyright\' "$tmp/members"\n\n'
    'rm -rf "$image_root/compliance/${alias}-${arch}"\n'
    'if IMAGE_ROOT="$image_root" OUT_DIR="$tmp/no-sbom" ZSTD_LEVEL=1 ZSTD_THREADS=1 \\\n'
)
if text.count(old) != 1:
    raise SystemExit("package contract member anchor mismatch")
text = text.replace(old, new, 1)
text = text.replace(
    'grep -q \'missing M2Image SBOM\' "$tmp/no-sbom.out"',
    'grep -q \'missing M2Image compliance bundle\' "$tmp/no-sbom.out"',
    1,
)
p.write_text(text, encoding="utf-8")

# Add focused source-evidence parser coverage.
p = Path("scripts/test_m2image_sbom.py")
text = p.read_text(encoding="utf-8")
footer = '\n\nif __name__ == "__main__":\n'
if footer not in text:
    raise SystemExit("SBOM test footer missing")
tests = r'''

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
'''
p.write_text(text.replace(footer, tests + footer, 1), encoding="utf-8")
