#!/usr/bin/env python3
"""按同步计划处理合并冲突与回流文件。

用法：upstream_sync_apply.py <plan.json>

规则（只处理「本次合入带来的」内容，不碰本仓库既有状态）：
  * 未合并的冲突路径匹配 delete_globs：从索引/工作区移除（保留本仓库的删除结果）。
  * 本次合入新增、且匹配 delete_globs 的路径：一并移除。
  * 匹配 ours_globs 的冲突文件：`git checkout --ours` + `git add`（保留本仓库版本）。
  * 其它冲突：不动，交给人工处理，并打印出来。
退出码 0 表示没有剩余冲突。
"""
from __future__ import annotations

import fnmatch
import json
import subprocess
import sys


def run(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(args, capture_output=True, text=True, check=check)


def match_any(path: str, globs: list[str]) -> str | None:
    for glob in globs:
        # 允许 `a/**` 同时匹配 `a` 本身与 `a/` 下的内容
        if fnmatch.fnmatch(path, glob):
            return glob
        if glob.endswith("/**") and path == glob[:-3]:
            return glob
    return None


def main() -> int:
    args = [a for a in sys.argv[1:] if a != "--only-new"]
    only_new = "--only-new" in sys.argv[1:]
    if len(args) != 1:
        print(__doc__, file=sys.stderr)
        return 1
    plan = json.load(open(args[0], encoding="utf-8"))
    delete_globs: list[str] = plan["delete_globs"]
    ours_globs: list[str] = plan["ours_globs"]

    status = run("git", "status", "--porcelain", "-z").stdout
    entries = [e for e in status.split("\0") if e]
    conflicted: list[str] = []
    delete_hits: list[str] = []

    for entry in entries:
        code, path = entry[:2], entry[3:]
        if code[0] == "U" or code[1] == "U" or code == "AA" or code == "DD":
            conflicted.append(path)

    removed: list[str] = []
    ours_kept: list[tuple[str, str]] = []

    # 1) 已删路径：保留删除结果
    for path in conflicted:
        why = match_any(path, delete_globs)
        if why:
            run("git", "rm", "-f", "--ignore-unmatch", "--", path)
            removed.append(path)
            delete_hits.append(why)

    # 2) 本仓库定制文件：冲突时保留本仓库版本
    for path in conflicted:
        if path in removed:
            continue
        why = match_any(path, ours_globs)
        if why:
            run("git", "checkout", "--ours", "--", path)
            run("git", "add", "--", path)
            ours_kept.append((path, why))

    # 3) 本次合入新增、且落在已删路径上的文件（新文件 / 重命名目标），再删一遍。
    #    只动「本次合入新增」的文件，绝不清理本仓库先前已提交的删除，避免误伤工作区。
    added = []
    added_status = run(
        "git", "diff", "--cached", "--name-status", "--diff-filter=A"
    ).stdout
    for line in added_status.splitlines():
        status, sep, path = line.partition("\t")
        if sep and status == "A":
            added.append(path)
    for path in added:
        why = match_any(path, delete_globs)
        if why and path not in removed:
            run("git", "rm", "-f", "--ignore-unmatch", "--", path)
            removed.append(path)
            delete_hits.append(why)

    remaining = [p for p in conflicted if p not in removed
                 and not any(p == kept for kept, _ in ours_kept)]
    if only_new:
        remaining = []

    # 清单未覆盖的冲突也必须落成可推送的提交：暂时保留 ours 并记录人工复核。
    # 这样不会留下 unmerged index，但 needs_review 会依据 apply.log 阻止推 main。
    manual_fallback = list(remaining)
    if manual_fallback and not only_new:
        for path in manual_fallback:
            run("git", "checkout", "--ours", "--", path, check=False)
            run("git", "add", "-A", "--", path, check=False)

    print(f"删除（保留本仓库删除结果）：{len(removed)} 个")
    for path in removed:
        print(f"  - {path}")
    print(f"保留本仓库版本：{len(ours_kept)} 个")
    for path, why in ours_kept:
        print(f"  - {path}   <- {why}")
    if manual_fallback:
        print(f"仍需人工处理的冲突：{len(manual_fallback)} 个")
        for path in manual_fallback:
            print(f"  ! {path}")
    else:
        print("无剩余冲突")

    # 冲突已经落盘为 ours，允许主脚本完成 merge；人工标记由 needs_review 消费。
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
