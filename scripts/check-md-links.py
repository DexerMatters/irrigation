#!/usr/bin/env python3
"""Fail when a relative Markdown link in the repository does not resolve."""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PATTERN = re.compile(r"\[[^\]]*\]\(([^)\s]+)\)")
SKIP_PREFIXES = ("http://", "https://", "mailto:", "#")
SKIP_DIRS = {"node_modules", "dist", "target", ".scratch", ".git"}


def documents():
    for base in (ROOT / "docs", ROOT / "plans", ROOT / "web"):
        for path in base.rglob("*.md"):
            if SKIP_DIRS.isdisjoint(path.parts):
                yield path
    yield ROOT / "README.md"


def main() -> int:
    broken = []
    for document in documents():
        for target in PATTERN.findall(document.read_text()):
            if target.startswith(SKIP_PREFIXES):
                continue
            path = (document.parent / target.split("#", 1)[0]).resolve()
            if not path.exists():
                broken.append(f"{document.relative_to(ROOT)}: {target}")
    for line in broken:
        print(f"broken link: {line}")
    print(f"{len(broken)} broken link(s)")
    return 1 if broken else 0


if __name__ == "__main__":
    sys.exit(main())
