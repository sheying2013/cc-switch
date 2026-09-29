#!/usr/bin/env python3
"""返回 0 表示无需人工复核，1 表示存在待复核项。

用法：upstream_sync_needs_review.py <scan.json> <apply.log>
"""
from __future__ import annotations

import json
import sys


def main() -> int:
    scan = json.load(open(sys.argv[1], encoding="utf-8"))
    manual = 0
    try:
        for line in open(sys.argv[2], encoding="utf-8", errors="replace"):
            if line.startswith("  ! "):
                manual += 1
    except OSError:
        pass
    return 0 if not scan.get("review_count") and not manual else 1


if __name__ == "__main__":
    raise SystemExit(main())
