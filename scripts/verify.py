#!/usr/bin/env python3
"""Runs the gates and proves they produced results.

Written after a stretch where `cargo test --workspace` failed to *compile* two test targets and a
`grep -c "test result: FAILED"` reported zero failures -- because a build that never compiles emits
no results at all. Absence of failures is not evidence of success; this script requires a floor of
passing suites as well as no failures, so a broken build cannot pass for a green one.

    scripts/verify.py              every gate
    scripts/verify.py --quick      fmt, clippy and the named package's tests
    scripts/verify.py --quick -p irrigation-syntax
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# The workspace ran 48 suites when this floor was set. A drop means targets vanished, not that
# they passed -- raise the floor when suites are added, never lower it.
SUITE_FLOOR = 46


def run(command: list[str], label: str) -> tuple[bool, str]:
    print(f"== {label}", flush=True)
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    output = result.stdout + result.stderr
    if result.returncode != 0:
        print(output[-4000:])
        return False, output
    print("   ok", flush=True)
    return True, output


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--quick", action="store_true", help="one package instead of the workspace")
    parser.add_argument("-p", "--package", default="irrigation", help="package for --quick")
    args = parser.parse_args()

    ok = True
    ok &= run(["cargo", "fmt", "--all", "--check"], "format")[0]
    ok &= run(
        [
            "cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--locked",
            "--", "-D", "warnings", "-W", "clippy::missing_panics_doc",
        ],
        "clippy (all features)",
    )[0]

    if args.quick:
        test = ["cargo", "test", "-p", args.package, "--locked"]
    else:
        test = ["cargo", "test", "--workspace", "--locked"]
    passed, output = run(test, "tests")
    ok &= passed

    # The count is the evidence. Zero failures with zero suites is a build that never ran.
    suites = len(re.findall(r"^test result: ok\.", output, re.MULTILINE))
    failed = len(re.findall(r"^test result: FAILED\.", output, re.MULTILINE))
    print(f"   {suites} passing suite(s), {failed} failing")
    if failed:
        ok = False
    if not args.quick and suites < SUITE_FLOOR:
        print(f"   only {suites} suites ran; the floor is {SUITE_FLOOR} -- a target is missing or the build broke")
        ok = False

    if not args.quick:
        ok &= run(["python3", "scripts/check-md-links.py"], "markdown links")[0]
        ok &= run(["python3", "scripts/check-doc-comments.py"], "doc comments")[0]
        ok &= run(
            ["cargo", "clippy", "-p", "irrigation-runtime", "--no-default-features", "--all-targets",
             "--locked", "--", "-D", "warnings"],
            "clippy (observation off)",
        )[0]

    print("== result")
    print("   PASS" if ok else "   FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
