#!/usr/bin/env python3
"""在合并结果上自动剥离「已删功能回流」的代码。

用法：upstream_sync_cleanup.py <repo-root> [--apply]

默认只报告（dry-run）。加 --apply 才真正改文件。

处理的是确定性、低风险的删除：
  1. 删除被禁模块的声明行        mod mcp; / pub mod skill;
  2. 删除命令注册行              commands::get_mcp_status,
  3. 删除顶层函数/结构体/测试模块 pub fn get_mcp_status(...) { ... }
  4. 删除 import 行              use crate::mcp::...;
  5. 配合清单里的 del 规则删除新增文件（由 upstream_sync_apply.py 负责）

删除后留下的引用会由 upstream_sync_scan.py 再次捕获。若仍有残留，同步流程会
拒绝自动推 main，改为开 PR 人工处理。
"""
from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys

# ---------------------------------------------------------------------------
# 规则
# ---------------------------------------------------------------------------

BANNED_MODULES = ("mcp", "prompt", "prompt_files", "gemini_mcp", "skill", "skills",
                  "pi_prompt_files")

# 顶层项目的首个声明行
ITEM_START = re.compile(
    r"^(?:pub\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+\"C\"\s+)?"
    r"(?:fn|struct|enum|trait|impl|const|static|type|union)\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)"
)

# 已删类型的确切名字（用于 import / interface / struct 判定）
BANNED_TYPE_NAMES = {
    "McpService", "SkillService", "PromptService", "SkillStore",
    "McpServer", "McpApps", "SkillApps", "InstalledSkill", "UnmanagedSkill",
    "McpStatus", "McpRoot", "PromptRoot", "McpConfig", "McpValidation",
    "McpServerSpec", "McpServersMap", "McpConfigResponse", "OpenCodeMcpServerSpec",
    "McpFormModal", "McpWizardModal", "UnifiedMcpPanel", "PiNativePromptResources",
    "PiPromptPanel", "PromptFormPanel", "PromptLibrary", "PromptListItem",
    "PromptPanel", "PromptToggle", "SkillCard", "SkillsPage", "UnifiedSkillsPanel",
    "RepoManagerPanel",
}

# 这些组件属于「供应商弹窗 / OAuth 登录中心」，必须保留，不能当回流删掉
KEEP_COMPONENT_NAMES = {
    "AuthSettingsPanel", "AuthCenterPanel", "CodexAuthSettings",
    "CopilotAuthSection", "CodexOAuthSection", "XaiOAuthSection",
}


def _is_protected(name: str) -> bool:
    return name in KEEP_COMPONENT_NAMES

# 已删的顶层函数（精确匹配，不按子串猜——fork 里保留的配置适配函数
# 例如 get_mcp_servers_yaml / strip_grok_mcp_servers_from_settings 不能误删）
BANNED_FN_NAMES = {
    "get_mcp_status", "read_mcp_json", "upsert_mcp_server", "delete_mcp_server",
    "read_mcp_servers_map", "set_mcp_servers_map", "validate_command_in_path",
    "get_skills_migration_result", "list_installed_skills", "install_skill",
    "uninstall_skill", "get_prompts", "save_prompt", "delete_prompt",
}

# 顶部 import 里出现这些模块即视为回流
BANNED_MODULE_HOSTS = (
    "mcp::", "crate::mcp", "crate::prompt", "crate::prompt_files",
    "crate::gemini_mcp", "services::mcp", "services::skill", "services::prompt",
    "commands::mcp", "commands::skill", "commands::prompt",
    "dao::mcp", "dao::skills", "dao::prompts",
)


def name_is_banned(name: str) -> bool:
    if _is_protected(name):
        return False
    return name in BANNED_TYPE_NAMES or name in BANNED_FN_NAMES


def module_decl_line(line: str) -> str | None:
    m = re.match(r"^\s*(?:pub\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;", line)
    if not m:
        return None
    return m.group(1)


def import_is_banned(line: str) -> bool:
    """只删「引用了已删模块」的 rust import。"""
    s = line.strip()
    if not s.startswith(("use ", "pub use ", "pub(crate) use ")):
        return False
    return any(host in s for host in BANNED_MODULE_HOSTS)


def import_brings_banned_type(line: str) -> bool:
    """`use ...::McpServer;` 这类把已删类型引回来的 import 也要删。"""
    s = line.strip()
    if not s.startswith(("use ", "pub use ", "pub(crate) use ")):
        return False
    if "::" not in s:
        return False
    leaf = re.split(r"[;{]", s.rsplit("::", 1)[-1])[0].strip()
    return leaf in BANNED_TYPE_NAMES


REG_ENTRY = re.compile(
    r"^\s*(?:commands::)?([A-Za-z_][A-Za-z0-9_]*)\s*,\s*(?://.*)?$"
)


def command_reg_is_banned(line: str) -> bool:
    """匹配 generate_handler! 里的一行注册项（有无 commands:: 前缀都算）。"""
    m = REG_ENTRY.match(line)
    if not m:
        return False
    name = m.group(1).lower()
    return any(k in name for k in ("mcp", "skill", "prompt"))


def find_registration_block(lines: list[str]) -> tuple[int, int] | None:
    """返回 generate_handler![...] 的 [起, 止) 行号区间。"""
    start = None
    for i, line in enumerate(lines):
        if "generate_handler![" in line:
            start = i
            break
    if start is None:
        return None
    depth = 0
    for i in range(start, len(lines)):
        depth += lines[i].count("[") - lines[i].count("]")
        if depth <= 0:
            return (start, i + 1)
    return None


def find_item_end(lines: list[str], start: int) -> int | None:
    """从顶层项目的声明行起，用花括号配平找到结束行（含）。

    声明可能跨多行（如 `pub fn foo()` 换行后才跟 `{`），所以先吃签名，
    遇到 `{` 再开始配平；没有花括号的项目用 `;` 结束。
    """
    depth = 0
    opened = False
    for i in range(start, min(len(lines), start + 4000)):
        code = strip_comment_and_string(lines[i])
        if not opened:
            if "{" in code:
                opened = True
                depth = code.count("{") - code.count("}")
                if depth <= 0:
                    return i
            elif ";" in code:
                return i
            continue
        depth += code.count("{") - code.count("}")
        if depth <= 0:
            return i
    return None


def strip_comment_and_string(line: str) -> str:
    out = []
    i = 0
    in_str = False
    while i < len(line):
        c = line[i]
        n = line[i + 1] if i + 1 < len(line) else ""
        if in_str:
            if c == "\\":
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        if c == "/" and n == "/":
            break
        if c == '"':
            in_str = True
            i += 1
            continue
        out.append(c)
        i += 1
    return "".join(out)


# ---------------------------------------------------------------------------
# 主逻辑
# ---------------------------------------------------------------------------

def process_rust(path: str, rel: str) -> list[tuple[int, str]]:
    """返回 [(行号, 被删内容)]，同时原地改写文件。"""
    lines = open(path, encoding="utf-8", errors="replace").read().split("\n")
    removed: list[tuple[int, str]] = []
    keep = [True] * len(lines)

    # 1) 模块声明 / use 行
    for i, line in enumerate(lines):
        mod = module_decl_line(line)
        if mod and mod in BANNED_MODULES:
            keep[i] = False
            removed.append((i + 1, line.strip()))
            continue
        if import_is_banned(line) or import_brings_banned_type(line):
            keep[i] = False
            removed.append((i + 1, line.strip()))

    # 2) 命令注册行（只在 generate_handler! 区间内处理）
    block = find_registration_block(lines)
    if block:
        for i in range(block[0], block[1]):
            if keep[i] and command_reg_is_banned(lines[i]):
                keep[i] = False
                removed.append((i + 1, lines[i].strip()))

    # 3) 已删功能的顶层项目（含 #[cfg(test)] mod xxx_tests）
    i = 0
    while i < len(lines):
        line = lines[i]
        if not keep[i]:
            i += 1
            continue
        if line.startswith(" ") or line.startswith("\t"):
            i += 1
            continue

        is_test_mod = bool(re.match(r"^\s*mod\s+[A-Za-z0-9_]*test[A-Za-z0-9_]*\s*\{", line))
        m = ITEM_START.match(line)
        if not (is_test_mod or m):
            i += 1
            continue

        end = find_item_end(lines, i)
        if end is None:
            i += 1
            continue

        block_lines = lines[i:end + 1]
        joined = "\n".join(block_lines)
        should_drop = False
        if is_test_mod:
            # 只删「名字就是已删功能」的测试模块，避免误杀保留代码的测试
            mod_name = re.match(r"^\s*mod\s+([A-Za-z0-9_]+)", line)
            head = joined[:4000]
            named = mod_name and any(
                k in mod_name.group(1).lower() for k in ("mcp", "skill", "prompt")
            )
            references = sum(
                1 for n in BANNED_TYPE_NAMES if re.search(rf"\b{re.escape(n)}\b", head)
            )
            if named and references >= 2:
                should_drop = True
        elif m and name_is_banned(m.group(1)):
            should_drop = True

        if should_drop:
            # 属性属于紧随其后的函数/类型；一并删除，避免把 #[tauri::command]
            # 留给下一个函数。模块级 doc/comment 不在这里自动删除。
            block_start = i
            while block_start > 0 and keep[block_start - 1] and lines[block_start - 1].lstrip().startswith("#["):
                block_start -= 1
            for j in range(block_start, end + 1):
                keep[j] = False
            removed.append((block_start + 1, lines[block_start].strip() + " … }"))
            i = end + 1
            continue
        i += 1

    # 4) 收尾
    if removed:
        out = [line for idx, line in enumerate(lines) if keep[idx]]
        text = "\n".join(out)
        text = re.sub(r"\n{3,}", "\n\n", text)
        open(path, "w", encoding="utf-8").write(text)
    return removed


def process_typescript(path: str) -> list[tuple[int, str]]:
    """删除已删功能的完整 import 语句与整块 interface/type 定义。"""
    lines = open(path, encoding="utf-8", errors="replace").read().split("\n")
    removed: list[tuple[int, str]] = []
    keep = [True] * len(lines)
    i = 0
    while i < len(lines):
        s = lines[i].strip()
        if not s.startswith(("import ", "export ", "} from ")):
            i += 1
            continue
        # 多行 import：从 import 起始行吃到 from 行，避免留下 import { } from ... 的残片。
        if s.startswith("import ") and " from " not in s:
            j = i
            while j < len(lines) and " from " not in lines[j]:
                j += 1
            if j < len(lines):
                block = "\n".join(lines[i:j + 1])
                bad_path = bool(
                    re.search(r"@/components/(?:mcp|skills|prompts)[/'\"]", block)
                    or re.search(r"@/lib/api/(?:mcp|skills|prompts)[/'\"]", block)
                )
                bad_name = any(
                    re.search(rf"\b{re.escape(word)}\b", block)
                    for word in BANNED_TYPE_NAMES
                )
                if bad_path:
                    for k in range(i, j + 1):
                        keep[k] = False
                    removed.append((i + 1, lines[i].strip()))
                    i = j + 1
                    continue
                if bad_name:
                    # 混合导入不能整个删掉（比如 McpServer + Provider）；只删
                    # 独占一行的已删类型。复杂语法交给残留扫描，禁止自动通过。
                    for k in range(i + 1, j):
                        if any(re.fullmatch(rf"\s*(?:type\s+)?{re.escape(word)}\s*,?\s*", lines[k])
                               for word in BANNED_TYPE_NAMES):
                            keep[k] = False
                            removed.append((k + 1, lines[k].strip()))
                    i = j + 1
                    continue
        # 单行 import / export-from
        if re.search(r"@/components/(?:mcp|skills|prompts)[/'\"]", s) or \
           re.search(r"@/lib/api/(?:mcp|skills|prompts)[/'\"]", s) or \
           re.search(r"@/components/settings/AuthSettings", s):
            keep[i] = False
            removed.append((i + 1, s))
            i += 1
            continue
        if re.search(r"\bfrom\b", s):
            banned = [word for word in BANNED_TYPE_NAMES if re.search(rf"\b{re.escape(word)}\b", s)]
            if banned:
                # 留存其它命名导入：只处理单独导入已删类型的语句。
                spec = s.split("from", 1)[0]
                identifiers = re.findall(r"\b[A-Za-z_][A-Za-z0-9_]*\b", spec)
                identifiers = [x for x in identifiers if x not in {"import", "export", "type", "as"}]
                if identifiers and all(x in BANNED_TYPE_NAMES for x in identifiers):
                    keep[i] = False
                    removed.append((i + 1, s))
        i += 1

    # 整块 interface / type 定义（花括号配平）
    i = 0
    while i < len(lines):
        if not keep[i]:
            i += 1
            continue
        m = re.match(r"^\s*export\s+(?:interface|type)\s+([A-Za-z0-9_]+)", lines[i])
        if m and name_is_banned(m.group(1)):
            depth = 0
            j = i
            seen_open = False
            while j < len(lines):
                code = strip_comment_and_string(lines[j])
                depth += code.count("{") - code.count("}")
                if "{" in code:
                    seen_open = True
                if seen_open and depth <= 0:
                    break
                if not seen_open and ";" in code:
                    break
                j += 1
            for k in range(i, min(j + 1, len(lines))):
                keep[k] = False
            removed.append((i + 1, lines[i].strip()))
            i = j + 1
            continue
        i += 1

    if removed:
        out = [line for idx, line in enumerate(lines) if keep[idx]]
        text = re.sub(r"\n{3,}", "\n\n", "\n".join(out))
        open(path, "w", encoding="utf-8").write(text)
    return removed


SKIP_DIRS = {".git", "node_modules", "target", "dist", "build", ".upstream-sync"}


def iter_files(root: str):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        for name in filenames:
            yield os.path.join(dirpath, name)


def remove_dead_mod_decls(root: str, removed_paths: set[str]) -> int:
    """只清理由本轮实际删除的模块文件留下的声明，绝不全仓猜测死模块。

    对 #[path] 和目录形式的模块，无法安全判断时不自动删除；CI 会检查。
    """
    removed = 0
    src_root = os.path.join(root, "src-tauri", "src")
    if not os.path.isdir(src_root):
        return 0
    for dirpath, dirnames, filenames in os.walk(src_root):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        for name in filenames:
            if not name.endswith(".rs"):
                continue
            path = os.path.join(dirpath, name)
            lines = open(path, encoding="utf-8", errors="replace").read().split("\n")
            keep = [True] * len(lines)
            for i, line in enumerate(lines):
                mod = module_decl_line(line)
                if not mod:
                    continue
                prev = lines[i - 1].strip() if i > 0 else ""
                if '#[path' in prev or '#[path' in line:
                    continue
                module_file = os.path.join(dirpath, mod + ".rs")
                module_dir = os.path.join(dirpath, mod, "mod.rs")
                if os.path.exists(module_file) or os.path.exists(module_dir):
                    continue
                relative_targets = {
                    os.path.relpath(module_file, root).replace(os.sep, "/"),
                    os.path.relpath(module_dir, root).replace(os.sep, "/"),
                }
                if relative_targets.isdisjoint(removed_paths):
                    continue
                keep[i] = False
                print(f"  {os.path.relpath(path, root)}:{i + 1}  删除本轮死模块声明  mod {mod};")
                removed += 1
            if not all(keep):
                text = re.sub(r"\n{3,}", "\n\n",
                              "\n".join(l for idx, l in enumerate(lines) if keep[idx]))
                open(path, "w", encoding="utf-8").write(text)
    return removed


def remove_new_offending_files(root: str, old_ref: str, new_ref: str, scan_path: str) -> set[str]:
    """删除「本次改动新增、且第一方扫描命中」的文件（只删新增，不碰既有文件）。"""
    diff = subprocess.run(
        ("git", "diff", "--name-status", old_ref, new_ref),
        cwd=root, capture_output=True, text=True, check=False,
    ).stdout
    added_paths = {
        line.split("\t", 1)[1]
        for line in diff.splitlines()
        if line.startswith("A\t") and "\t" in line
    }
    if not added_paths:
        return set()
    # 已删功能的 token 在文件里出现 -> 该新增文件属于回流
    try:
        scan = json.load(open(scan_path, encoding="utf-8"))
    except OSError:
        return set()

    removed: set[str] = set()
    for finding in scan.get("review", []):
        rel = finding["path"]
        if rel not in added_paths:
            continue
        full = os.path.join(root, rel)
        if os.path.exists(full):
            os.remove(full)
            print(f"  {rel}  删除（本次新增且含回流代码）")
            removed.add(rel)
    return removed


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("repo")
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--remove-new-offenders", metavar="OLD..NEW",
                    help="删除本次新增且含回流 token 的文件，格式 <old-ref>..<new-ref>")
    ap.add_argument("--scan", help="--remove-new-offenders 用的 scan.json")
    ap.add_argument("--apply-log", help="首次路径过滤的日志，包含本轮删除的文件")
    args = ap.parse_args()
    root = os.path.abspath(args.repo)

    if args.remove_new_offenders:
        old_ref, new_ref = args.remove_new_offenders.split("..", 1)
        removed = remove_new_offending_files(root, old_ref, new_ref, args.scan)
        if args.apply_log:
            with open(args.apply_log, encoding="utf-8") as log:
                removed.update(line[4:].strip() for line in log if line.startswith("  - ") and "   <- " not in line)
        dead_mods = remove_dead_mod_decls(root, removed)
        print(f"新增回流文件删除数量：{len(removed)}；顺带清理死模块声明：{dead_mods}")
        return 0

    if not args.apply:
        # dry-run：用与 --apply 完全相同的判定，只报告不落盘
        total = 0
        for path in iter_files(root):
            rel = os.path.relpath(path, root)
            if not path.endswith((".rs", ".ts", ".tsx")):
                continue
            try:
                lines = open(path, encoding="utf-8", errors="replace").read().split("\n")
            except OSError:
                continue
            hits: list[tuple[int, str]] = []
            if path.endswith(".rs"):
                block = find_registration_block(lines)
                for i, line in enumerate(lines):
                    mod = module_decl_line(line)
                    if mod and mod in BANNED_MODULES:
                        hits.append((i + 1, line.strip()))
                    elif import_is_banned(line) or import_brings_banned_type(line):
                        hits.append((i + 1, line.strip()))
                    elif block and block[0] <= i < block[1] and command_reg_is_banned(line):
                        hits.append((i + 1, line.strip()))
                    else:
                        m = ITEM_START.match(line)
                        if m and name_is_banned(m.group(1)):
                            hits.append((i + 1, line.strip()))
            else:
                for i, line in enumerate(lines):
                    stripped = line.strip()
                    if stripped.startswith(("import ", "export ", "} from ")):
                        if any(
                            re.search(rf"\b{re.escape(w)}\b", stripped)
                            for w in BANNED_TYPE_NAMES
                        ) and "from" in stripped:
                            hits.append((i + 1, stripped))
                    m = re.match(r"^\s*export\s+(?:interface|type)\s+([A-Za-z0-9_]+)", line)
                    if m and name_is_banned(m.group(1)):
                        hits.append((i + 1, stripped))
            if hits:
                print(f"  [dry-run] {rel}: {len(hits)} 处")
                total += len(hits)
        print(f"dry-run 合计 {total} 处（未修改文件）")
        return 0

    removed_total = 0
    for path in iter_files(root):
        rel = os.path.relpath(path, root)
        if path.endswith(".rs"):
            removed = process_rust(path, rel)
        elif path.endswith((".ts", ".tsx")):
            removed = process_typescript(path)
        else:
            continue
        for lineno, content in removed:
            print(f"  {rel}:{lineno}  删除  {content[:100]}")
            removed_total += 1
    print(f"共删除 {removed_total} 处回流代码")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
