#!/usr/bin/env python3
"""解析 .github/upstream-sync.conf，输出同步计划 JSON。

用法：upstream_sync_plan.py <conf>  >  plan.json
"""
from __future__ import annotations

import json
import sys

VALID_ACTIONS = {"del", "keep-deleted", "redact", "brand", "follow"}


def parse_conf(path: str) -> list[dict]:
    sections: list[dict] = []
    current: dict | None = None
    with open(path, encoding="utf-8") as fh:
        for raw in fh:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            if line == "[section]":
                current = {"paths": [], "action": "redact", "name": "", "reason": ""}
                sections.append(current)
                continue
            if current is None:
                raise SystemExit(f"{path}: 第一行必须是 [section]")
            key, _, value = line.partition("=")
            key = key.strip()
            value = value.strip()
            if key == "path":
                current["paths"].append(value)
            elif key in ("name", "reason", "action"):
                if key == "action" and value not in VALID_ACTIONS:
                    raise SystemExit(f"{path}: 未知 action={value}，可选 {sorted(VALID_ACTIONS)}")
                current[key] = value
            else:
                raise SystemExit(f"{path}: 未知键 {key}")
    for sec in sections:
        if not sec["paths"]:
            raise SystemExit(f"{path}: section {sec['name']!r} 没有任何 path")
    return sections


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 1
    sections = parse_conf(sys.argv[1])

    delete_globs: list[str] = []
    ours_globs: list[str] = []
    follow_globs: list[str] = []
    for sec in sections:
        target = {
            "del": delete_globs,
            "keep-deleted": delete_globs,
            "redact": ours_globs,
            "brand": ours_globs,
            "follow": follow_globs,
        }[sec["action"]]
        target.extend(sec["paths"])

    def dedupe(items: list[str]) -> list[str]:
        seen: set[str] = set()
        out: list[str] = []
        for item in items:
            if item not in seen:
                seen.add(item)
                out.append(item)
        return out

    print(json.dumps({
        "sections": sections,
        "delete_globs": dedupe(delete_globs),
        "ours_globs": dedupe(ours_globs),
        "follow_globs": dedupe(follow_globs),
        "base_nonempty": 1,
    }, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
