//! 规则文件的 serde 结构。
//!
//! `deny_unknown_fields` 是刻意开启的：规则库是产品的核心资产，
//! 拼错的字段必须立刻报错，而不是被静默忽略后产生一条永不命中的规则。

use serde::Deserialize;

use crate::model::{Action, DataClass, Risk};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRulesFile {
    pub schema_version: u32,
    /// 机器可读标识，如 `claude-code`
    pub agent: String,
    /// 展示名，如 `Claude Code`
    pub display_name: String,
    /// 该 agent 的运行进程名（小写、不含路径）。命中时其目录一律跳过清理。
    #[serde(default)]
    pub processes: Vec<String>,
    #[serde(default)]
    pub roots: Vec<RootDef>,
    #[serde(default)]
    pub rules: Vec<RuleDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootDef {
    pub id: String,
    /// 支持 `~` 前缀
    pub path: String,
    /// 特征文件/目录；全部不存在则认为该 agent 未安装
    #[serde(default)]
    pub probe: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDef {
    /// 全局唯一，建议 `<agent>.<class>.<name>`
    pub id: String,
    /// glob 模式，支持 `~` 前缀；`*` 不跨目录，`**` 跨目录
    pub path: String,
    /// 未声明时视为 unknown（fail-safe，默认保留）
    #[serde(default)]
    pub class: Option<DataClass>,
    #[serde(default)]
    pub risk: Option<Risk>,
    #[serde(default)]
    pub action: Option<Action>,
    #[serde(default)]
    pub note: Option<String>,
    /// 仅当文件修改时间早于 N 天才命中；用于「90 天前的旧会话」
    #[serde(default)]
    pub mtime_older_than_days: Option<u64>,
}

impl RuleDef {
    pub fn effective_class(&self) -> DataClass {
        self.class.unwrap_or(DataClass::Unknown)
    }

    /// 未显式声明 action 时，按类别取最保守的默认值。
    pub fn effective_action(&self) -> Action {
        if let Some(a) = self.action {
            return a;
        }
        match self.effective_class() {
            DataClass::Asset => Action::Protect,
            DataClass::Ephemeral => Action::Clean,
            DataClass::Workspace | DataClass::Product | DataClass::Unknown => Action::Review,
        }
    }

    pub fn effective_risk(&self) -> Risk {
        if let Some(r) = self.risk {
            return r;
        }
        match self.effective_class() {
            DataClass::Asset => Risk::Critical,
            DataClass::Ephemeral | DataClass::Product => Risk::Medium,
            DataClass::Workspace | DataClass::Unknown => Risk::High,
        }
    }
}
