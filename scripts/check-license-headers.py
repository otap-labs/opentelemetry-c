#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0

"""Check or add SPDX identifiers to eligible repository files."""

from __future__ import annotations

import argparse
import subprocess
from pathlib import Path


SPDX = "SPDX-License-Identifier: Apache-2.0"
EXCLUDED = {
    "Cargo.lock",
    "LICENSE",
    "api/exported-symbols.txt",
    "sdk/api-import-symbols.txt",
    "sdk/exported-symbols.txt",
    "fuzz/Cargo.lock",
}


def comment_for(path: Path) -> str | None:
    """Return the SPDX comment for an eligible file, or None when excluded."""
    relative = path.as_posix()
    if relative in EXCLUDED:
        return None

    if path.name in {"CMakeLists.txt", "Makefile", "Dockerfile", ".gitignore"}:
        return f"# {SPDX}"

    if path.name.endswith((".cmake.in", ".pc.in")):
        return f"# {SPDX}"

    suffix = path.suffix.lower()
    if suffix in {".rs", ".c", ".h", ".cpp"}:
        return f"// {SPDX}"
    if suffix in {".py", ".sh", ".toml", ".yaml", ".yml", ".cmake"}:
        return f"# {SPDX}"
    if suffix == ".md":
        return f"<!-- {SPDX} -->"
    return None


def tracked_files(root: Path) -> list[Path]:
    output = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout
    return [root / item.decode() for item in output.split(b"\0") if item]


def has_identifier(text: str) -> bool:
    return SPDX in "\n".join(text.splitlines()[:5])


def add_identifier(path: Path, text: str, comment: str) -> None:
    lines = text.splitlines(keepends=True)
    insert_at = 1 if lines and lines[0].startswith("#!") else 0
    lines.insert(insert_at, f"{comment}\n")
    if insert_at + 1 < len(lines) and lines[insert_at + 1].strip():
        lines.insert(insert_at + 1, "\n")
    path.write_text("".join(lines), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--fix",
        action="store_true",
        help="add missing identifiers instead of only reporting them",
    )
    args = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    missing: list[Path] = []
    for path in tracked_files(root):
        comment = comment_for(path.relative_to(root))
        if comment is None:
            continue
        text = path.read_text(encoding="utf-8")
        if has_identifier(text):
            continue
        if args.fix:
            add_identifier(path, text, comment)
        else:
            missing.append(path.relative_to(root))

    if missing:
        print("Missing Apache-2.0 SPDX identifier:")
        for path in missing:
            print(f"  {path}")
        print("Run scripts/check-license-headers.py --fix to update eligible files.")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
