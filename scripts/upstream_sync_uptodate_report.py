#!/usr/bin/env python3
"""Print the "already up to date" report for the upstream sync workflow."""
from __future__ import annotations

import argparse
import datetime as dt
import subprocess


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--base", required=True)
    ap.add_argument("--ours", required=True)
    args = ap.parse_args()

    def short(ref: str) -> str:
        out = subprocess.run(
            ("git", "rev-parse", "--short", ref),
            cwd=args.repo, capture_output=True, text=True, check=False,
        )
        return out.stdout.strip()

    print("# 上游同步报告")
    print()
    print(f"- 生成时间：{dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')}")
    print(f"- 上游基线：`{args.base}` (`{short(args.base)}`)")
    print(f"- 本仓库定制：`{args.ours}` (`{short(args.ours)}`)")
    print()
    print("## 结论")
    print()
    print("已是最新，没有需要同步的上游提交。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
