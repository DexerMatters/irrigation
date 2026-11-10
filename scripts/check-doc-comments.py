#!/usr/bin/env python3
"""Refuses a doc comment that an attribute splits in two.

Rustdoc concatenates an item's `///` lines in source order, so this damage renders
correctly and nothing else complains: the file compiles, every test passes, clippy and
the doc gate are silent, and the only symptom is source that reads as prose interrupted
by an attribute. It is easy to produce with an edit that spans a doc block.

    /// Summary.
    #[must_use]
    /// # Panics        <- the prose resumes after an attribute
    pub fn f() {}

The attribute belongs below the last doc line.
"""

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def scan(path: Path) -> list[tuple[int, str]]:
    try:
        lines = path.read_text(encoding="utf-8").split("\n")
    except (OSError, UnicodeDecodeError):
        return []
    findings = []
    for index in range(len(lines) - 2):
        first, second, third = (line.strip() for line in lines[index : index + 3])
        if first.startswith("///") and second.startswith("#[") and third.startswith("///"):
            findings.append((index + 1, second))
    return findings


def main() -> int:
    targets = sorted(ROOT.glob("crates/**/*.rs"))
    total = 0
    for path in targets:
        for line, attribute in scan(path):
            print(
                f"{path.relative_to(ROOT)}:{line}: a doc comment resumes after {attribute};"
                " move the attribute below the last doc line"
            )
            total += 1
    # A scan that looked at nothing would report success. Say what it covered.
    print(f"checked {len(targets)} Rust files, {total} split doc comment(s)")
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main())
