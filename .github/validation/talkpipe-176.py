#!/usr/bin/env python3
import hashlib
import importlib.util
import json
import tempfile
from pathlib import Path

from talkpipe.pipe.core import segment, source

ROOT = Path("/work")


def load_module(name: str, rel: str):
    path = ROOT / rel
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


sbom = load_module("m2image_sbom", "scripts/m2image_sbom.py")
compliance = load_module("m2image_compliance", "scripts/m2image_compliance.py")
GPL = ROOT / "licenses/GPL-2.0-only.txt"

SCENARIOS = [
    {"name": "complete-alpine", "expected": "pass", "kind": "complete"},
    {"name": "empty-guest-legal", "expected": "not-ready", "kind": "empty-legal"},
    {"name": "missing-source-identity", "expected": "reject", "kind": "missing-source"},
    {"name": "malformed-dpkg-source", "expected": "reject", "kind": "bad-dpkg"},
    {"name": "wrong-spdx-version", "expected": "reject", "kind": "bad-spdx"},
    {"name": "deterministic-repeat", "expected": "pass", "kind": "deterministic"},
    {"name": "tampered-license-evidence", "expected": "reject", "kind": "tamper"},
]


def alpine_document():
    packages = sbom.parse_alpine(
        "P:busybox\nV:1.37.0-r31\nA:x86_64\nL:GPL-2.0-only\n"
        "o:busybox\nc:0123456789abcdef0123456789abcdef01234567\n\n"
        "P:linux-virt\nV:6.18.44-r0\nA:x86_64\nL:GPL-2.0-only\n"
        "o:linux-lts\nc:fedcba9876543210fedcba9876543210fedcba98\n"
    )
    return sbom.make_spdx(
        distribution="alpine",
        image_alias="alpine-3.24.1",
        image_version="3.24.1",
        architecture="x86_64",
        packages=packages,
    )


def make_legal(root: Path):
    (root / "usr/share/spdx/text").mkdir(parents=True, exist_ok=True)
    (root / "usr/share/licenses/busybox").mkdir(parents=True, exist_ok=True)
    (root / "usr/share/spdx/text/MIT.txt").write_text("MIT license text\n", encoding="utf-8")
    (root / "usr/share/spdx/text/GPL-2.0-only.txt").write_text("GPL corpus text\n", encoding="utf-8")
    (root / "usr/share/licenses/busybox/COPYING").write_text("busybox notice\n", encoding="utf-8")


def verify_index(bundle: Path):
    index = json.loads((bundle / "licenses/index.json").read_text(encoding="utf-8"))
    for item in index["files"]:
        path = bundle / item["bundlePath"]
        if not path.is_file():
            raise ValueError(f"indexed legal file missing: {item['bundlePath']}")
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if digest != item["sha256"]:
            raise ValueError(f"indexed legal file hash mismatch: {item['bundlePath']}")


@source()
def adversarial_cases():
    yield from SCENARIOS


@segment()
def exercise(cases):
    for case in cases:
        outcome = "pass"
        detail = ""
        try:
            with tempfile.TemporaryDirectory() as tmpdir:
                tmp = Path(tmpdir)
                legal = tmp / "guest"
                legal.mkdir()
                doc = alpine_document()

                if case["kind"] in {"complete", "deterministic", "tamper"}:
                    make_legal(legal)
                elif case["kind"] == "missing-source":
                    doc["packages"][1].pop("comment", None)
                elif case["kind"] == "bad-dpkg":
                    sbom.parse_dpkg(
                        "Package: linux-image-virtual\nStatus: install ok installed\n"
                        "Architecture: amd64\nVersion: 7.0.0-30.30\n"
                        "Source: linux-meta (unterminated\n"
                    )
                elif case["kind"] == "bad-spdx":
                    doc["spdxVersion"] = "SPDX-2.2"

                sbom_path = tmp / "sbom.spdx.json"
                sbom_path.write_text(json.dumps(doc, sort_keys=True), encoding="utf-8")
                out = tmp / "bundle"
                summary = compliance.build_bundle(
                    spdx_path=sbom_path,
                    legal_root=legal,
                    gpl2_text=GPL,
                    output_dir=out,
                )

                if summary["guestLegalFileCount"] == 0:
                    outcome = "not-ready"
                    detail = "bundle contains no guest-provided legal evidence"
                else:
                    source_map = json.loads((out / "source-map.json").read_text(encoding="utf-8"))
                    if not source_map["packages"]:
                        raise ValueError("source map unexpectedly empty")
                    verify_index(out)

                if case["kind"] == "deterministic":
                    second = tmp / "bundle-2"
                    compliance.build_bundle(
                        spdx_path=sbom_path,
                        legal_root=legal,
                        gpl2_text=GPL,
                        output_dir=second,
                    )
                    for rel in ("bundle.json", "source-map.json", "licenses/index.json", "README.txt"):
                        if (out / rel).read_bytes() != (second / rel).read_bytes():
                            raise ValueError(f"non-deterministic artifact: {rel}")

                if case["kind"] == "tamper":
                    target = out / "licenses/guest/usr/share/spdx/text/MIT.txt"
                    target.write_text("tampered\n", encoding="utf-8")
                    verify_index(out)

        except (ValueError, AssertionError, OSError, json.JSONDecodeError) as exc:
            outcome = "reject"
            detail = str(exc)

        yield {
            "name": case["name"],
            "expected": case["expected"],
            "outcome": outcome,
            "detail": detail,
        }


results = list(adversarial_cases() | exercise())
failed = False
for result in results:
    print(
        f"{result['name']}: expected={result['expected']} got={result['outcome']}"
        + (f" ({result['detail']})" if result['detail'] else "")
    )
    failed |= result["expected"] != result["outcome"]

if failed:
    raise SystemExit("TalkPipe #176 adversary found classification mismatches")
print(f"TalkPipe #176 adversary passed: {len(results)} scenarios")
