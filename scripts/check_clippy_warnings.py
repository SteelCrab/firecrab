#!/usr/bin/env python3
"""Gate Clippy warnings against a small, reviewable baseline."""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections import Counter
from pathlib import Path
from urllib.parse import urlparse


SECTIONS = ("packages", "lints")


def package_name(package_id: str, target: dict | None = None) -> str:
    """Extract a human-readable crate name from Cargo's package_id."""
    target = target or {}
    if not package_id:
        return target.get("name", "unknown")

    # Legacy Cargo package IDs: `crate 0.1.0 (path+file:///...)`.
    if " (" in package_id:
        return package_id.split(" ", 1)[0]

    source, sep, fragment = package_id.rpartition("#")
    if sep:
        # Registry IDs: `registry+...#crate@1.2.3`.
        if "@" in fragment:
            name, _version = fragment.rsplit("@", 1)
            if name:
                return name

        # Some path package IDs carry the crate name in the fragment.
        if fragment and not fragment[0].isdigit():
            return fragment

        # Path IDs may carry only the version after `#`; use the path basename.
        if source.startswith("path+"):
            parsed = urlparse(source[len("path+"):])
            name = Path(parsed.path).name
            if name:
                return name

    return target.get("name") or package_id


def warning_key(message: dict) -> tuple:
    diagnostic = message["message"]
    primary = next(
        (span for span in diagnostic.get("spans", []) if span.get("is_primary")),
        {},
    )
    code = (diagnostic.get("code") or {}).get("code") or "unknown"
    package = package_name(message.get("package_id", ""), message.get("target"))
    return (
        package,
        code,
        diagnostic.get("message", ""),
        primary.get("file_name", ""),
        primary.get("line_start", 0),
        primary.get("column_start", 0),
        primary.get("line_end", 0),
        primary.get("column_end", 0),
    )


def collect(path: str | Path) -> dict:
    """Read Cargo JSON messages and return de-duplicated warning counts."""
    seen: set[tuple] = set()
    packages: Counter[str] = Counter()
    lints: Counter[str] = Counter()

    with open(path, encoding="utf-8") as stream:
        for line_number, line in enumerate(stream, 1):
            if not line.strip():
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError as exc:
                raise ValueError(
                    f"{path}:{line_number}: invalid Cargo JSON: {exc.msg}"
                ) from exc

            if message.get("reason") != "compiler-message":
                continue
            diagnostic = message.get("message") or {}
            if diagnostic.get("level") != "warning":
                continue

            key = warning_key(message)
            if key in seen:
                continue
            seen.add(key)
            packages[key[0]] += 1
            lints[key[1]] += 1

    return {
        "total": len(seen),
        "packages": dict(sorted(packages.items())),
        "lints": dict(sorted(lints.items())),
    }


def validate_counts(counts: dict, label: str) -> None:
    """Reject malformed or internally inconsistent stats/baselines."""
    if not isinstance(counts, dict):
        raise ValueError(f"{label}: expected an object")
    if not isinstance(counts.get("total"), int) or counts["total"] < 0:
        raise ValueError(f"{label}: total must be a non-negative integer")

    for section in SECTIONS:
        values = counts.get(section)
        if not isinstance(values, dict):
            raise ValueError(f"{label}: {section} must be an object")
        for name, value in values.items():
            if not isinstance(name, str) or not name:
                raise ValueError(f"{label}: {section} keys must be non-empty strings")
            if not isinstance(value, int) or value < 0:
                raise ValueError(
                    f"{label}: {section}.{name} must be a non-negative integer"
                )
        if sum(values.values()) != counts["total"]:
            raise ValueError(
                f"{label}: {section} counts sum to {sum(values.values())}, "
                f"not total {counts['total']}"
            )


def load_baseline(path: str | Path) -> dict:
    with open(path, encoding="utf-8") as stream:
        baseline = json.load(stream)
    validate_counts(baseline, str(path))
    return baseline


def compare(current: dict, baseline: dict) -> tuple[list[str], list[str]]:
    """Return (regressions, stale_baseline) messages."""
    validate_counts(current, "current warnings")
    validate_counts(baseline, "baseline")

    regressions: list[str] = []
    stale: list[str] = []

    if current["total"] > baseline["total"]:
        regressions.append(
            f"total warnings increased: {baseline['total']} -> {current['total']}"
        )
    elif current["total"] < baseline["total"]:
        stale.append(
            f"total warnings decreased: {baseline['total']} -> {current['total']}"
        )

    nouns = {"packages": "crate", "lints": "lint"}
    for section in SECTIONS:
        names = sorted(set(current[section]) | set(baseline[section]))
        for name in names:
            before = baseline[section].get(name, 0)
            after = current[section].get(name, 0)
            if after > before:
                regressions.append(
                    f"{nouns[section]} {name}: warnings increased {before} -> {after}"
                )
            elif after < before:
                stale.append(
                    f"{nouns[section]} {name}: warnings decreased {before} -> {after}"
                )

    return regressions, stale


def delta(current: int, baseline: int) -> str:
    value = current - baseline
    return f"{value:+d}"


def summary_markdown(
    current: dict,
    baseline: dict,
    regressions: list[str],
    stale: list[str],
) -> str:
    lines = [
        "### Clippy warnings",
        "",
        (
            f"**{current['total']} warnings** — baseline {baseline['total']} "
            f"(Δ {delta(current['total'], baseline['total'])})"
        ),
        "",
        "#### By crate",
        "",
        "| Crate | Baseline | Current | Δ |",
        "| --- | ---: | ---: | ---: |",
    ]

    for name in sorted(set(current["packages"]) | set(baseline["packages"])):
        before = baseline["packages"].get(name, 0)
        after = current["packages"].get(name, 0)
        lines.append(f"| `{name}` | {before} | {after} | {delta(after, before)} |")

    lines.extend(
        [
            "",
            "#### By lint",
            "",
            "| Lint | Baseline | Current | Δ |",
            "| --- | ---: | ---: | ---: |",
        ]
    )

    for name in sorted(set(current["lints"]) | set(baseline["lints"])):
        before = baseline["lints"].get(name, 0)
        after = current["lints"].get(name, 0)
        lines.append(f"| `{name}` | {before} | {after} | {delta(after, before)} |")

    lines.append("")
    if regressions:
        lines.append("**Status: ❌ new Clippy warnings detected.**")
        lines.extend(f"- {item}" for item in regressions)
    elif stale:
        lines.append("**Status: ⚠️ warning baseline is stale.**")
        lines.append(
            "Warnings were removed; regenerate and commit the baseline in this PR."
        )
        lines.extend(f"- {item}" for item in stale)
    else:
        lines.append("**Status: ✅ warning baseline unchanged.**")

    return "\n".join(lines) + "\n"


def write_baseline(path: str | Path, current: dict) -> None:
    validate_counts(current, "current warnings")
    with open(path, "w", encoding="utf-8") as stream:
        json.dump(current, stream, indent=2, sort_keys=True)
        stream.write("\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Fail when Clippy warnings drift from the checked-in baseline."
    )
    parser.add_argument("messages", help="Cargo --message-format=json output")
    parser.add_argument("baseline", help="checked-in warning baseline JSON")
    parser.add_argument(
        "--write-baseline",
        action="store_true",
        help="replace the baseline with counts from the supplied Cargo JSON",
    )
    args = parser.parse_args(argv)

    try:
        current = collect(args.messages)
        validate_counts(current, "current warnings")
        if args.write_baseline:
            write_baseline(args.baseline, current)
            print(
                f"wrote {current['total']} warnings to baseline {args.baseline}",
                file=sys.stderr,
            )
            return 0

        baseline = load_baseline(args.baseline)
        regressions, stale = compare(current, baseline)
        summary = summary_markdown(current, baseline, regressions, stale)
        print(summary, end="")

        summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary_path:
            with open(summary_path, "a", encoding="utf-8") as stream:
                stream.write(summary)

        if regressions:
            print(
                "\nClippy warning regression. Fix the new warning(s), or if the "
                "change is intentional review and update the baseline.",
                file=sys.stderr,
            )
            return 1
        if stale:
            print(
                "\nClippy warnings were removed. Refresh the baseline with:\n"
                f"  python3 {Path(__file__).as_posix()} {args.messages} "
                f"{args.baseline} --write-baseline",
                file=sys.stderr,
            )
            return 1
        return 0
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"clippy warning gate: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
