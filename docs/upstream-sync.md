# 上游同步（全自动，保留本仓库的删减与定制）

本仓库基于 [farion1231/cc-switch](https://github.com/farion1231/cc-switch)，但移除了
MCP / Skills / Prompt 全部功能、设置-认证、赞助与推广信息，并改名 `cc switch live`。

直接从 GitHub 点 **Sync fork** 会把这些东西全部带回来。本仓库自带一条全自动同步流水线，
**上游一有更新就会自动合并、自动剥离回流代码、自动推送**，不需要人工介入。

```
上游更新
  └─> fetch
      └─> merge
          └─> 按清单保留本仓库的删除/定制
              └─> 循环自动剥离回流代码（最多 5 轮）
                  ├─ 扫描无残留 -> 先跑 CI，成功后才推 main
                  └─ 仍有残留   -> 开 PR（main 不受影响）
```

## 调度

`.github/workflows/upstream-sync.yml`

- **每周一 18:00 UTC（北京时间周二 02:00）自动跑**
- 也可以在 Actions 页面手动触发（支持 `dry_run` 只看报告）

## 自动判定

| 脚本退出码 | 含义 | 流水线动作 |
| --- | --- | --- |
| `0` | 自动剥离后无残留 | 推送同步分支，显式运行 CI；CI 成功后再推 main |
| `2` | 存在无法安全自动剥离的残留 | 推送 `upstream-sync` 分支并开 PR，main 不受影响 |
| `1` | 用法/环境错误 | 工作流失败 |

`0` 这条路径就是「无人值守」：上游合并进来、被删功能被剥离掉、结果直接进 main。

## 自动剥离做什么

`scripts/upstream_sync_cleanup.py` 会循环执行，直到扫描不再发现回流：

1. **删除上游新增的已删功能文件**（含清单 `del` 规则与「新增且含回流 token」的文件）
2. **删除死模块声明** `mod new_mcp_thing;`（指向已不存在文件的声明）
3. **删除命令注册行** `commands::get_mcp_status,`
4. **删除已删功能的顶层函数/结构体** `pub fn get_mcp_status(...) { ... }`
5. **删除引用了已删模块的 import 行** `use crate::mcp::...;`
6. **删除前端引用了已删组件的 import 与已删类型定义**

### 明确的保护名单

下面的代码是**故意保留**的，自动化绝不会删（有专门的白名单与回归验证）：

| 保留内容 | 原因 |
| --- | --- |
| `claude_mcp.rs` 的 onboarding 函数 | 「跳过 Claude Code 初次安装确认」仍在用 |
| `AuthSettingsPanel` / `AuthCenterPanel` / `CodexAuthSettings` | 供应商弹窗里的 OAuth 登录，不是设置页认证 |
| `grok_config.rs` / `hermes_config.rs` / `opencode_config.rs` 里的 MCP 配置读写函数 | 供应商配置适配，不是被删的 MCP 管理功能 |
| `tests/golden/main.rs` 的 `mod support;` | 用 `#[path]` 指定路径，不是死引用 |

## 本地手动同步

```bash
scripts/upstream-sync.sh --dry-run              # 只看计划
scripts/upstream-sync.sh                        # 同步到 upstream-sync 分支
scripts/upstream-sync.sh --base refs/remotes/upstream/main
```

退出码同上：`0` 干净，`2` 需人工复核，`1` 错误。

## 报告

每次同步都会写 `.upstream-sync/report.md`（被 gitignore）：

| 区块 | 含义 |
| --- | --- |
| 本次同步的上游提交 | 带进来的上游提交 |
| 自动处理结果 | 删了哪些、哪些文件保留了本仓库版本、剩余冲突 |
| 待人工处理的冲突 | 清单没覆盖的冲突 |
| 待人工复核的集成点 | 自动剥离后仍残留的回流代码 |

`.upstream-sync/cleanup.log` 记录每一步自动剥离了哪一行的什么内容。

## 维护过滤清单

`.github/upstream-sync.conf`，纯文本：

```ini
[section]
name=deleted-feature-modules
action=del          # del | keep-deleted | redact | brand | follow
reason=删除原因（只用于报告）
path=src-tauri/src/mcp/**     # 一行一个路径或 glob
```

- 又删了别的功能：新增一个 `action=del` 的 section。
- 某个文件想回归上游实现：放进 `action=follow`。
- `path` 支持 `*`、`?`、`[]`、`**`。

改完先跑 `scripts/upstream-sync.sh --dry-run` 看解析结果。

## 已知边界

- **不编译验证。** 流水线只保证「代码结构与本仓库的删减一致」，不保证编译通过。
  最终把关靠同步工作流显式 dispatch 的 CI（`.github/workflows/ci.yml`）。
  如果 CI 红了，说明上游改动的语义需要人工介入。
- **上游改进同名文件会被放弃。** `lib.rs`、`app_config.rs` 这类文件是「冲突时保留本仓库版本」，
  上游对它们的改动会被丢弃并记在报告里。这是刻意的取舍。
- **扫描基于符号名。** 上游若用全新名字重新实现同类功能，扫描抓不到，需要靠 CI 或人工复审。
- **历史文档不参与判定。** `CHANGELOG.md`、`docs/release-notes/**` 中的上游地址与赞助记录按约定保留。
