use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

use crate::config::{copy_file, get_app_config_dir, get_app_config_path, write_json_file};
use crate::error::AppError;
use crate::provider::ProviderManager;

/// 应用类型
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppType {
    Claude,
    #[serde(
        rename = "claude-desktop",
        alias = "claude_desktop",
        alias = "claudeDesktop"
    )]
    ClaudeDesktop,
    Codex,
    Gemini,
    GrokBuild,
    OpenCode,
    OpenClaw,
    Hermes,
    Pi,
    Mcode,
}

impl AppType {
    pub fn as_str(&self) -> &str {
        match self {
            AppType::Claude => "claude",
            AppType::ClaudeDesktop => "claude-desktop",
            AppType::Codex => "codex",
            AppType::Gemini => "gemini",
            AppType::GrokBuild => "grokbuild",
            AppType::OpenCode => "opencode",
            AppType::OpenClaw => "openclaw",
            AppType::Hermes => "hermes",
            AppType::Pi => "pi",
            AppType::Mcode => "mcode",
        }
    }

    /// Check if this app uses additive mode
    ///
    /// - Switch mode (false): Only the current provider is written to live config (Claude, Codex, Gemini)
    /// - Additive mode (true): Providers coexist in native config and can be enabled independently
    ///   (OpenCode, OpenClaw, Hermes, Pi)
    pub fn is_additive_mode(&self) -> bool {
        matches!(
            self,
            AppType::OpenCode | AppType::OpenClaw | AppType::Hermes | AppType::Pi | AppType::Mcode
        )
    }

    pub fn supports_local_proxy(&self) -> bool {
        matches!(
            self,
            AppType::Claude | AppType::Codex | AppType::Gemini | AppType::GrokBuild
        )
    }

    /// Return an iterator over all app types
    pub fn all() -> impl Iterator<Item = AppType> {
        [
            AppType::Claude,
            AppType::ClaudeDesktop,
            AppType::Codex,
            AppType::Gemini,
            AppType::GrokBuild,
            AppType::OpenCode,
            AppType::OpenClaw,
            AppType::Hermes,
            AppType::Pi,
            AppType::Mcode,
        ]
        .into_iter()
    }
}

impl FromStr for AppType {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_lowercase();
        match normalized.as_str() {
            "claude" => Ok(AppType::Claude),
            "claude-desktop" | "claude_desktop" | "claudedesktop" => Ok(AppType::ClaudeDesktop),
            "codex" => Ok(AppType::Codex),
            "gemini" => Ok(AppType::Gemini),
            "grokbuild" | "grok-build" | "grok_build" | "grok" => Ok(AppType::GrokBuild),
            "opencode" => Ok(AppType::OpenCode),
            "openclaw" => Ok(AppType::OpenClaw),
            "hermes" => Ok(AppType::Hermes),
            "pi" => Ok(AppType::Pi),
            "mcode" => Ok(AppType::Mcode),
            other => Err(AppError::localized(
                "unsupported_app",
                format!("不支持的应用标识: '{other}'。可选值: claude, claude-desktop, codex, gemini, grokbuild, opencode, openclaw, hermes, pi。"),
                format!("Unsupported app id: '{other}'. Allowed: claude, claude-desktop, codex, gemini, grokbuild, opencode, openclaw, hermes, pi."),
            )),
        }
    }
}

/// 通用配置片段（按应用分治）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommonConfigSnippets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openclaw: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermes: Option<String>,
}

impl CommonConfigSnippets {
    /// 获取指定应用的通用配置片段
    pub fn get(&self, app: &AppType) -> Option<&String> {
        match app {
            AppType::Claude => self.claude.as_ref(),
            AppType::ClaudeDesktop => None,
            AppType::Codex => self.codex.as_ref(),
            AppType::Gemini => self.gemini.as_ref(),
            AppType::GrokBuild => None,
            AppType::OpenCode => self.opencode.as_ref(),
            AppType::OpenClaw => self.openclaw.as_ref(),
            AppType::Hermes => self.hermes.as_ref(),
            AppType::Pi | AppType::Mcode => None,
        }
    }

    /// 设置指定应用的通用配置片段
    pub fn set(&mut self, app: &AppType, snippet: Option<String>) {
        match app {
            AppType::Claude => self.claude = snippet,
            AppType::ClaudeDesktop => {}
            AppType::Codex => self.codex = snippet,
            AppType::Gemini => self.gemini = snippet,
            AppType::GrokBuild => {}
            AppType::OpenCode => self.opencode = snippet,
            AppType::OpenClaw => self.openclaw = snippet,
            AppType::Hermes => self.hermes = snippet,
            AppType::Pi | AppType::Mcode => {}
        }
    }
}

/// 多应用配置结构（向后兼容）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAppConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    /// 应用管理器（claude/codex）
    #[serde(flatten)]
    pub apps: HashMap<String, ProviderManager>,
    /// 通用配置片段（按应用分治）
    #[serde(default)]
    pub common_config_snippets: CommonConfigSnippets,
    /// Claude 通用配置片段（旧字段，用于向后兼容迁移）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_common_config_snippet: Option<String>,
}

fn default_version() -> u32 {
    2
}

impl Default for MultiAppConfig {
    fn default() -> Self {
        let mut apps = HashMap::new();
        apps.insert("claude".to_string(), ProviderManager::default());
        apps.insert("claude-desktop".to_string(), ProviderManager::default());
        apps.insert("codex".to_string(), ProviderManager::default());
        apps.insert("gemini".to_string(), ProviderManager::default());
        apps.insert("grokbuild".to_string(), ProviderManager::default());
        apps.insert("opencode".to_string(), ProviderManager::default());
        apps.insert("openclaw".to_string(), ProviderManager::default());
        apps.insert("hermes".to_string(), ProviderManager::default());
        apps.insert("pi".to_string(), ProviderManager::default());
        apps.insert("mcode".to_string(), ProviderManager::default());

        Self {
            version: 2,
            apps,
            common_config_snippets: CommonConfigSnippets::default(),
            claude_common_config_snippet: None,
        }
    }
}

impl MultiAppConfig {
    /// 从文件加载配置（仅支持 v2 结构）
    pub fn load() -> Result<Self, AppError> {
        let config_path = get_app_config_path();

        if !config_path.exists() {
            log::info!("配置文件不存在，创建新的多应用配置");
            let config = Self::default();
            // 立即保存到磁盘
            config.save()?;
            return Ok(config);
        }

        // 尝试读取文件
        let content =
            std::fs::read_to_string(&config_path).map_err(|e| AppError::io(&config_path, e))?;

        // 先解析为 Value，以便严格判定是否为 v1 结构；
        // 满足：顶层同时包含 providers(object) + current(string)，且不包含 version/apps 关键键，即视为 v1
        let mut value: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| AppError::json(&config_path, e))?;
        let is_v1 = value.as_object().is_some_and(|map| {
            let has_providers = map.get("providers").map(|v| v.is_object()).unwrap_or(false);
            let has_current = map.get("current").map(|v| v.is_string()).unwrap_or(false);
            // v1 的充分必要条件：有 providers 和 current，且 apps 不存在
            let has_apps = map.contains_key("apps");
            has_providers && has_current && !has_apps
        });
        if is_v1 {
            return Err(AppError::localized(
                "config.unsupported_v1",
                "检测到旧版 v1 配置格式。当前版本已不再支持运行时自动迁移。\n\n解决方案：\n1. 安装 v3.2.x 版本进行一次性自动迁移\n2. 或手动编辑 ~/.cc-switch/config.json，将顶层结构调整为：\n   {\"version\": 2, \"claude\": {...}, \"codex\": {...}}\n\n",
                "Detected legacy v1 config. Runtime auto-migration is no longer supported.\n\nSolutions:\n1. Install v3.2.x for one-time auto-migration\n2. Or manually edit ~/.cc-switch/config.json to adjust the top-level structure:\n   {\"version\": 2, \"claude\": {...}, \"codex\": {...}}\n\n",
            ));
        }

        // 已移除功能的顶层分组（mcp / prompts / skills 等）必须在反序列化前剔除：
        // apps 使用 #[serde(flatten)]，任何未知顶层键都会被当成应用条目去解析成
        // ProviderManager，形状不符时会让整份配置直接加载失败（历史遗留配置打不开）。
        let known_apps: std::collections::HashSet<String> =
            AppType::all().map(|app| app.as_str().to_string()).collect();
        let mut pruned_groups: Vec<String> = Vec::new();
        if let Some(map) = value.as_object_mut() {
            map.retain(|key, _| {
                let keep = known_apps.contains(key)
                    || matches!(
                        key.as_str(),
                        "version" | "common_config_snippets" | "claude_common_config_snippet"
                    );
                if !keep {
                    pruned_groups.push(key.clone());
                }
                keep
            });
        }
        for key in &pruned_groups {
            log::info!("丢弃配置中已移除的顶层分组: {key}");
        }

        // 解析 v2 结构
        let mut config: Self =
            serde_json::from_value(value).map_err(|e| AppError::json(&config_path, e))?;
        let mut updated = !pruned_groups.is_empty();

        // 旧版把 mcp / prompts / skills 作为顶层字段写进 config.json；这些字段已移除。
        // 因为 apps 用了 #[serde(flatten)]，它们会被解析成“应用”条目。这里按真实
        // AppType 白名单清掉这些幽灵条目，避免它们在下次保存时被写回文件。
        let orphan_apps: Vec<String> = config
            .apps
            .keys()
            .filter(|key| !known_apps.contains(*key))
            .cloned()
            .collect();
        for key in orphan_apps {
            log::info!("丢弃配置中已废弃的顶层分组: {key}");
            config.apps.remove(&key);
            updated = true;
        }

        // 确保 gemini 应用存在（兼容旧配置文件）
        if !config.apps.contains_key("gemini") {
            config
                .apps
                .insert("gemini".to_string(), ProviderManager::default());
            updated = true;
        }

        // 迁移通用配置片段：claude_common_config_snippet → common_config_snippets.claude
        if let Some(old_claude_snippet) = config.claude_common_config_snippet.take() {
            log::info!(
                "迁移通用配置：claude_common_config_snippet → common_config_snippets.claude"
            );
            config.common_config_snippets.claude = Some(old_claude_snippet);
            updated = true;
        }

        if updated {
            log::info!("配置结构已更新，保存配置...");
            config.save()?;
        }

        Ok(config)
    }

    /// 保存配置到文件
    pub fn save(&self) -> Result<(), AppError> {
        let config_path = get_app_config_path();
        // 先备份旧版（若存在）到 ~/.cc-switch/config.json.bak，再写入新内容
        if config_path.exists() {
            let backup_path = get_app_config_dir().join("config.json.bak");
            if let Err(e) = copy_file(&config_path, &backup_path) {
                log::warn!("备份 config.json 到 .bak 失败: {e}");
            }
        }

        write_json_file(&config_path, self)?;
        Ok(())
    }

    /// 获取指定应用的管理器
    pub fn get_manager(&self, app: &AppType) -> Option<&ProviderManager> {
        self.apps.get(app.as_str())
    }

    /// 获取指定应用的管理器（可变引用）
    pub fn get_manager_mut(&mut self, app: &AppType) -> Option<&mut ProviderManager> {
        self.apps.get_mut(app.as_str())
    }

    /// 确保应用存在
    pub fn ensure_app(&mut self, app: &AppType) {
        if !self.apps.contains_key(app.as_str()) {
            self.apps
                .insert(app.as_str().to_string(), ProviderManager::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_type_parses_claude_desktop_aliases() {
        assert_eq!(
            "claude-desktop".parse::<AppType>().unwrap(),
            AppType::ClaudeDesktop
        );
        assert_eq!(
            "claude_desktop".parse::<AppType>().unwrap(),
            AppType::ClaudeDesktop
        );
        assert_eq!(
            "claudeDesktop".parse::<AppType>().unwrap(),
            AppType::ClaudeDesktop
        );
        assert_eq!(AppType::ClaudeDesktop.as_str(), "claude-desktop");
    }
}
