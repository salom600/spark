#!/usr/bin/env python3
"""spark line-count audit.

Counts non-blank, non-comment lines of Rust in the engine and editor crates
(excluding tests and generated code) and enforces the project's LOC budget:
  - target: <= 10,000 (ambitious)
  - ceiling: <= 15,000 (hard limit, CI fails above this)

WGSL shader lines are reported separately (they are GPU programs, not engine
logic, and excluded from the budget).

Usage:
    python3 tools/count_loc.py            # report
    python3 tools/count_loc.py --fail-over 15000   # enforce ceiling
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES = [
    ("spark (engine core)", ROOT / "crates/spark/src"),
    ("spark_macros (codegen)", ROOT / "crates/spark_macros/src"),
    ("spark_editor (editor+runner)", ROOT / "crates/spark_editor/src"),
]

TEST_BLOCK = re.compile(r"#\[cfg\(test\)\]")


def count_rust(path: Path) -> int:
    """Count non-blank, non-comment, non-test lines in one .rs file."""
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    count = 0
    in_block_comment = False
    in_test_block = False
    brace_depth_at_test = 0
    depth = 0
    for line in lines:
        stripped = line.strip()
        if in_block_comment:
            if "*/" in stripped:
                in_block_comment = False
            continue
        # Track test-module exclusion by brace matching from #[cfg(test)].
        if not in_test_block and TEST_BLOCK.match(stripped):
            in_test_block = True
            brace_depth_at_test = depth
            continue
        if stripped.startswith("/*"):
            if "*/" not in stripped:
                in_block_comment = True
            continue
        if not stripped or stripped.startswith("//"):
            # still count braces for comment lines that contain code? No —
            # comment lines are skipped, but braces inside comments would
            # break depth tracking; that is acceptable for our style (docs
            # use /// above code, braces in comments are rare).
            continue
        count += 1
        depth += line.count("{") - line.count("}")
        if in_test_block and depth <= brace_depth_at_test:
            # The test module closed; discount the mod line and stop.
            in_test_block = False
            count -= 1 if stripped.startswith("mod") else 0
            continue
        if in_test_block:
            count -= 1
    return max(count, 0)


def main() -> int:
    total = 0
    print("spark line-count audit (non-blank, non-comment, excl. tests)")
    print("-" * 62)
    for label, dirpath in CRATES:
        files = sorted(dirpath.rglob("*.rs"))
        n = sum(count_rust(f) for f in files)
        total += n
        print(f"{label:<38} {n:>6} lines  ({len(files)} files)")
    shader_files = sorted((ROOT / "crates/spark/src/render/shaders").glob("*.wgsl"))
    shader_lines = sum(
        sum(1 for l in f.read_text().splitlines() if l.strip() and not l.strip().startswith("//"))
        for f in shader_files
    )
    print("-" * 62)
    print(f"{'TOTAL core (Rust)':<38} {total:>6} lines")
    print(f"{'WGSL shaders (informational)':<38} {shader_lines:>6} lines")
    print(f"{'Budget':<38} {'10000 target / 15000 ceiling':>24}")
    ok = total <= 10000
    print(f"Status: {'WITHIN ambitious target' if ok else 'over target'}")
    if "--fail-over" in sys.argv:
        ceiling = int(sys.argv[sys.argv.index("--fail-over") + 1])
        if total > ceiling:
            print(f"FAIL: {total} > ceiling {ceiling}")
            return 1
        print(f"Ceiling check passed ({total} <= {ceiling})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
