//! 领域模型：所有分类判定的公共词汇表。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 数据的价值类别 —— 决定「能不能删」的根本依据。
///
/// 顺序即保守度顺序（声明顺序被 `Ord` 使用），输出报表时也按此顺序排列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataClass {
    /// 用户长期积累、删除不可恢复：skills、agents、memory、配置
    Asset,
    /// 用户自己产出的内容：工作区、项目、上传件。价值高但无法用规则判定，
    /// 必须逐项人工确认 —— 不能和「可重建产物」混为一谈
    Workspace,
    /// 由源码/工具链重建：dist、node_modules、target
    Product,
    /// 可再生或仅丢失历史：缓存、日志、会话转录、快照
    Ephemeral,
    /// 无任何规则命中，默认保留，永不进入清理计划
    Unknown,
}

impl DataClass {
    pub fn label(self) -> &'static str {
        match self {
            DataClass::Asset => "用户资产",
            DataClass::Workspace => "用户数据",
            DataClass::Ephemeral => "可重建垃圾",
            DataClass::Product => "项目产物",
            DataClass::Unknown => "未识别",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            DataClass::Asset => "不可恢复，禁止清理",
            DataClass::Workspace => "用户产出内容，需逐项确认",
            DataClass::Ephemeral => "删除仅丢失历史，可重建",
            DataClass::Product => "可由源码重新构建",
            DataClass::Unknown => "无规则命中，默认保留",
        }
    }

    /// 冲突消解时的保守度权重：越大越保守。
    pub fn conservativeness(self) -> u8 {
        match self {
            DataClass::Asset => 4,
            DataClass::Workspace => 3,
            DataClass::Product => 2,
            DataClass::Ephemeral => 1,
            DataClass::Unknown => 0,
        }
    }
}

/// 误删代价等级。`Ord` 由低到高，用于「取最保守」的比较。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
    Critical,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Low => "低",
            Risk::Medium => "中",
            Risk::High => "高",
            Risk::Critical => "极高",
        }
    }
}

/// 建议处置动作。优先级：Protect > SkipIfLocked > Review > Clean。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// 可清理
    Clean,
    /// 需人工确认
    Review,
    /// 可清理，但目标进程运行时必须跳过（如 sqlite 主库）
    SkipIfLocked,
    /// 硬保护，任何情况下不得进入清理计划
    Protect,
}

impl Action {
    pub fn priority(self) -> u8 {
        match self {
            Action::Clean => 1,
            Action::Review => 2,
            Action::SkipIfLocked => 3,
            Action::Protect => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Clean => "可清理",
            Action::Review => "需确认",
            Action::SkipIfLocked => "跳过占用",
            Action::Protect => "保护",
        }
    }
}

/// 单个文件的分类结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub path: PathBuf,
    pub size: u64,
    /// Unix 秒级时间戳
    pub mtime: Option<i64>,
    /// 所在的 agent 根（物理归属，不一定是规则的 agent）
    pub agent: String,
    pub class: DataClass,
    pub risk: Option<Risk>,
    pub action: Action,
    /// 命中的规则 id；`None` 表示无规则命中
    pub rule_id: Option<String>,
    pub note: Option<String>,
}

impl Node {
    pub fn is_protected(&self) -> bool {
        matches!(self.action, Action::Protect) || matches!(self.class, DataClass::Asset | DataClass::Unknown)
    }
}
