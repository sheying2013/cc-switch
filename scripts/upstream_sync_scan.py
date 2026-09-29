#!/usr/bin/env python3
"""扫描合并结果里是否重新出现「本仓库已删除功能」的集成点。

用法：upstream_sync_scan.py <repo-root>  >  scan.json

只报告，不修改任何文件。命中的一般是上游新增代码引用了我们删掉的模块、
命令、类型或赞助/推广字段——这些必须人工处理，否则编译或前端会炸。
"""
from __future__ import annotations

import json
import os
import re
import sys

# 跳过这些目录，避免把依赖/产物算进来
SKIP_DIRS = {
    ".git", "node_modules", "target", "dist", "build", ".upstream-sync",
    ".venv", "venv", "__pycache__", ".next", "coverage",
}

# 只看这些源码/配置后缀
WATCH_EXTS = {
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".json", ".toml", ".yml", ".yaml",
    ".md", ".html", ".mjs", ".sh",
}

# 自动生成或与功能取舍无关的文件
SKIP_FILES = {"Cargo.lock", "package-lock.json", "pnpm-lock.yaml", "yarn.lock"}

RULES: list[tuple[str, str, str]] = [
    # (分类, 正则, 说明)
    ("rust-service", r"\b(?:McpService|SkillService|PromptService|SkillStore)\b",
     "已删除的 Rust 服务类型被引用"),
    ("rust-type", r"\b(?:McpServer|McpApps|SkillApps|InstalledSkill|UnmanagedSkill|McpRoot|PromptRoot|McpConfig|McpStatus|McpValidation)\b",
     "已删除的 Rust 数据结构被引用"),
    ("rust-module", r"crate::(?:mcp|prompt|prompt_files|gemini_mcp)\b",
     "已删除的 Rust 模块被引用"),
    ("rust-cmd", r"\b(?:get_mcp_status|read_mcp_json|upsert_mcp_server|delete_mcp_server|"
                 r"read_mcp_servers_map|set_mcp_servers_map|get_skills_migration_result|"
                 r"list_installed_skills|install_skill|uninstall_skill|get_prompts|save_prompt|delete_prompt)\b",
     "已删除的后端命令被引用"),
    ("ts-type", r"\b(?:McpServerSpec|McpApps|McpServersMap|McpConfigResponse|"
                 r"OpenCodeMcpServerSpec|parseSmartMcpJson)\b",
     "已删除的前端类型/工具被引用"),
    ("ts-command", r"[\"'`](?:get_mcp_config|upsert_mcp_server_in_config|delete_mcp_server_in_config|"
                   r"set_mcp_enabled|import_mcp_from_(?:claude|codex)|get_skills_migration_result)[\"'`]",
     "已删除的命令名出现在前端调用或 mock 里"),
    ("settings-field", r"\b(?:skill_sync_method|skill_storage_location|skills_ssot_migration_(?:pending|snapshot))\b",
     "已删除的设置字段/迁移标记被引用"),
    ("sponsor", r"\b(?:isPartner|partnerPromotion|ccswitch\.io|ytag)\b|aff=",
     "赞助/推广/aff 相关内容重新出现"),
    ("settings-auth", r"\b(?:SettingsAuthPanel|AuthSettingsTab|AuthCenterPanel)\b",
     "设置-认证相关面板重新出现"),
    ("upstream-repo", r"farion1231/cc-switch",
     "上游仓库地址重新出现（更新源/文档链接应指向本仓库）"),
]

# 这些命中属于「有意保留的历史溯源注释」，不计入人工复核，只做提示
ALLOW_HINTS = {
    "upstream-repo": ("//", "#"),
}

# 有意保留、不算回流的命中： (分类, 路径前缀, 行内必须出现的子串或 None=任意)
SUPPRESSED: list[tuple[str, str, str | None]] = [
    # 供应商弹窗里的 OAuth 登录面板是供应商功能，不在「设置-认证」删除范围内
    ("settings-auth", "src/components/providers/", None),
    # AuthCenterPanel 是「供应商弹窗里的 OAuth 登录中心」，必须保留
    ("settings-auth", "src/components/settings/AuthCenterPanel.tsx", None),
    ("settings-auth", "tests/components/CodexOAuthSection.test.tsx", None),
    # 同步工具自身必须写上游仓库地址，属于有意保留
    ("upstream-repo", "scripts/upstream-sync.sh", None),
    ("upstream-repo", "scripts/upstream_sync_", None),
    ("upstream-repo", ".github/workflows/upstream-sync.yml", None),
    ("upstream-repo", ".github/upstream-sync.conf", None),
    ("settings-auth", "tests/components/EditProviderDialog.test.tsx", None),
    ("settings-auth", "tests/components/AddProviderDialog.test.tsx", None),
    # skills 迁移标记：DDL 与备份兼容校验按决策保留
    ("settings-field", "src-tauri/src/database/schema.rs", "skills_ssot_migration"),
    ("settings-field", "src-tauri/src/database/backup.rs", "skills_ssot_migration"),
    ("settings-field", "src-tauri/src/database/tests.rs", "skills_ssot_migration"),
]


def is_suppressed(category: str, rel: str, excerpt: str) -> bool:
    for cat, prefix, needle in SUPPRESSED:
        if category != cat:
            continue
        if not rel.startswith(prefix):
            continue
        if needle is None or needle in excerpt:
            return True
    return False


def relative(path: str, root: str) -> str:
    return os.path.relpath(path, root).replace(os.sep, "/")


def iter_files(root: str):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        for name in filenames:
            if name in SKIP_FILES:
                continue
            if os.path.splitext(name)[1].lower() not in WATCH_EXTS:
                continue
            yield os.path.join(dirpath, name)


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 1
    root = os.path.abspath(sys.argv[1])

    findings: list[dict] = []
    for path in iter_files(root):
        rel = relative(path, root)
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                lines = fh.read().split("\n")
        except OSError:
            continue
        for lineno, line in enumerate(lines, 1):
            for category, pattern, reason in RULES:
                m = re.search(pattern, line)
                if not m:
                    continue
                # 注释里的上游仓库链接属于有意保留，降级为提示
                stripped = line.lstrip()
                historical = (
                    rel.startswith("docs/")
                    or rel == "CHANGELOG.md"
                    or rel == "CONTRIBUTING.md"
                    or rel.startswith("docs/release-notes/")
                )
                if is_suppressed(category, rel, line):
                    continue
                if historical:
                    # 历史发布说明 / 变更日志按约定保留，只做提示不阻塞
                    level = "hint"
                elif category == "upstream-repo" and stripped.startswith(("//", "#", "///")):
                    level = "hint"
                else:
                    level = "review"
                findings.append({
                    "category": category,
                    "level": level,
                    "path": rel,
                    "line": lineno,
                    "token": m.group(0),
                    "reason": reason,
                    "excerpt": line.strip()[:200],
                })

    review = [f for f in findings if f["level"] == "review"]
    hints = [f for f in findings if f["level"] == "hint"]
    print(json.dumps({
        "review_count": len(review),
        "hint_count": len(hints),
        "review": review,
        "hints": hints,
    }, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
