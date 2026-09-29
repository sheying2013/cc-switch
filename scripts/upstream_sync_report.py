#!/usr/bin/env python3
"""生成上游同步的 Markdown 报告，并给出「能否直接合并」的结论。

用法：
  upstream_sync_report.py --plan plan.json --repo <root> --base <ref> --ours <ref>
                          --apply-log apply.log --scan scan.json --conflicted <0|1>
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import subprocess
from collections import Counter


def git(root: str, *args: str) -> str:
    return subprocess.run(
        ("git", *args), cwd=root, capture_output=True, text=True, check=False
    ).stdout.strip()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--plan", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--base", required=True)
    ap.add_argument("--ours", required=True)
    ap.add_argument("--apply-log", required=True)
    ap.add_argument("--scan", required=True)
    ap.add_argument("--conflicted", required=True)
    ap.add_argument("--dedupe-src", default=None)
    args = ap.parse_args()

    root = os.path.abspath(args.repo)
    plan = json.load(open(args.plan, encoding="utf-8"))
    scan = json.load(open(args.scan, encoding="utf-8"))
    scan_review = scan.get("review", [])
    scan_hint = scan.get("hints", [])

    upstream_log = git(root, "log", "--oneline", "--reverse", f"{args.ours}..{args.base}")
    conflicted_files = git(root, "diff", "--name-only", "--diff-filter=U")
    conflicted_count = len([l for l in conflicted_files.split("\n") if l])

    removed, ours_kept, manual = [], [], []
    for line in open(args.apply_log, encoding="utf-8", errors="replace"):
        line = line.rstrip("\n")
        if line.startswith("  - "):
            (ours_kept if "   <- " in line else removed).append(line[4:])
        elif line.startswith("  ! "):
            manual.append(line[4:])

    categories = Counter(f["category"] for f in scan_review)
    blocking = bool(manual) or bool(scan_review) or conflicted_count > 0

    base_sha = git(root, "rev-parse", "--short", args.base)
    ours_sha = git(root, "rev-parse", "--short", args.ours)

    out: list[str] = []
    out.append("# 上游同步报告")
    out.append("")
    out.append(f"- 生成时间：{dt.datetime.now().isoformat(timespec='seconds')}")
    out.append(f"- 上游基线：`{args.base}` (`{base_sha}`)")
    out.append(f"- 本仓库定制：`{args.ours}` (`{ours_sha}`)")
    out.append(f"- 未同步的上游提交：**{len([l for l in upstream_log.split(chr(10)) if l])}** 个")
    out.append(f"- 是否发生合并冲突：{'是' if args.conflicted == '1' else '否'}")
    out.append("")

    out.append("## 结论")
    out.append("")
    if not upstream_log:
        out.append("已是最新，无需同步。")
    elif blocking:
        out.append("**需要人工复核后才能合并。** 自动过滤已完成，但仍有下方列出的项需要处理，")
        out.append("直接合并可能导致编译失败或功能回流。")
    else:
        out.append("**自动过滤后无残留集成点，可提交并在 CI 通过后合并。**")
    out.append("")

    if upstream_log:
        out.append("## 本次同步的上游提交")
        out.append("")
        out.append("```")
        out.append(upstream_log)
        out.append("```")
        out.append("")

    out.append("## 自动处理结果")
    out.append("")
    out.append(f"- 按过滤清单删除（保留本仓库删除结果，action=del/keep-deleted）：**{len(removed)}** 个")
    out.append(f"- 冲突处保留本仓库版本（action=redact/brand）：**{len(ours_kept)}** 个")
    out.append(f"- 仍需人工处理的冲突：**{len(manual)}** 个")
    out.append("")

    if removed:
        out.append("<details><summary>已删除路径</summary>")
        out.append("")
        for path in removed:
            out.append(f"- `{path}`")
        out.append("")
        out.append("</details>")
        out.append("")

    if ours_kept:
        out.append("<details><summary>保留本仓库版本的文件</summary>")
        out.append("")
        for path in ours_kept:
            out.append(f"- `{path}`")
        out.append("")
        out.append("</details>")
        out.append("")

    if manual:
        out.append("## ⚠️ 待人工处理的冲突")
        out.append("")
        for path in manual:
            out.append(f"- `{path}`")
        out.append("")

    out.append("## 待人工复核的集成点（可能让已删功能回流）")
    out.append("")
    if not scan_review:
        out.append("无。扫描未发现 MCP / Skills / Prompt / 设置-认证 / 赞助推广 的新集成点。")
    else:
        out.append(f"共 **{len(scan_review)}** 处，按类别：")
        out.append("")
        for cat, count in categories.most_common():
            out.append(f"- `{cat}`：{count}")
        out.append("")
        out.append("| 文件:行 | 类别 | 命中 | 说明 |")
        out.append("| --- | --- | --- | --- |")
        for f in scan_review[:200]:
            excerpt = f["excerpt"].replace("|", "\\|")[:120]
            out.append(
                f"| `{f['path']}:{f['line']}` | `{f['category']}` | "
                f"`{f['token']}` | {f['reason']} — {excerpt} |"
            )
        if len(scan_review) > 200:
            out.append(f"| … | | | 另有 {len(scan_review) - 200} 处，见 `scan.json` |")
    out.append("")
    out.append(f"<sub>提示级命中（历史文档/来源注释，按约定保留）：{len(scan_hint)} 处，详见 `scan.json`。</sub>")
    out.append("")
    out.append("## 过滤清单")
    out.append("")
    out.append("| section | action | 路径数 | 说明 |")
    out.append("| --- | --- | --- | --- |")
    for sec in plan["sections"]:
        out.append(
            f"| `{sec['name']}` | `{sec['action']}` | {len(sec['paths'])} | {sec.get('reason','')} |"
        )
    out.append("")

    print("\n".join(out))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
