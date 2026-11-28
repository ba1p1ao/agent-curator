//! 清理计划。
//!
//! 计划是「扫描结果」与「实际动手」之间的唯一闸门：
//! 它是纯数据、可序列化、可 diff、可审查。执行器只吃 `Plan`，
//! 不直接接触扫描候选，避免任何"顺手多删一个"的可能。

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::model::{Action, DataClass, Node, Risk};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanOptions {
    /// 连带处理 action=review 的条目（高风险垃圾 / 用户数据 / 项目产物）
    pub include_review: bool,
    /// 允许处理正在运行的 agent 目录
    pub include_running: bool,
    /// 单项体积下限（字节）
    pub min_size: u64,
    /// 只处理这些规则 id（空 = 不限）
    pub rules: Vec<String>,
    /// 只处理这些 agent（空 = 不限）
    pub agents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub index: usize,
    pub path: PathBuf,
    pub bytes: u64,
    pub mtime: Option<i64>,
    pub agent: String,
    pub class: DataClass,
    pub risk: Risk,
    pub action: Action,
    pub rule_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedItem {
    pub path: PathBuf,
    pub bytes: u64,
    pub rule_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub generated_at: String,
    pub options: PlanOptions,
    pub running_agents: Vec<String>,
    pub items: Vec<PlanItem>,
    pub skipped: Vec<SkippedItem>,
}

impl Plan {
    pub fn total_bytes(&self) -> u64 {
        self.items.iter().map(|i| i.bytes).sum()
    }

    pub fn skipped_bytes(&self) -> u64 {
        self.skipped.iter().map(|i| i.bytes).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 只保留指定下标的条目，返回新计划。
    ///
    /// 下标来自同一次 `build` 产出的 `PlanItem.index`，越界下标直接忽略。
    /// 这是界面「条目级勾选」的落点：用户选了什么，执行器就只看到什么——
    /// 筛选发生在 `Plan` 这一层，而不是在文件操作层，避免"顺手多删"。
    pub fn retain(&self, keep: &[usize]) -> Plan {
        let set: std::collections::HashSet<usize> = keep.iter().copied().collect();
        let mut filtered = self.clone();
        filtered.items.retain(|item| set.contains(&item.index));
        filtered
    }

    /// 按规则汇总：规则 id -> (文件数, 字节数)
    pub fn by_rule(&self) -> BTreeMap<String, (u64, u64)> {
        let mut map: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        for item in &self.items {
            let key = item
                .rule_id
                .clone()
                .unwrap_or_else(|| "(无规则)".to_string());
            let entry = map.entry(key).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += item.bytes;
        }
        map
    }

    /// 跳过原因聚合，用于提示"为什么少了这么多"。
    pub fn skip_reasons(&self) -> BTreeMap<String, (u64, u64)> {
        let mut map: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        for item in &self.skipped {
            let entry = map.entry(item.reason.clone()).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += item.bytes;
        }
        map
    }
}

/// 由候选集生成计划。纯函数，不碰文件系统。
pub fn build(candidates: &[Node], running: &[String], options: &PlanOptions) -> Plan {
    let mut items: Vec<PlanItem> = Vec::new();
    let mut skipped: Vec<SkippedItem> = Vec::new();

    for node in candidates {
        // 明确不属于清理范围的数据，连"跳过"都不必记录
        if matches!(node.class, DataClass::Asset | DataClass::Unknown)
            || node.action == Action::Protect
        {
            continue;
        }

        if !options.rules.is_empty() {
            let matched = node
                .rule_id
                .as_ref()
                .map(|r| options.rules.iter().any(|want| want == r))
                .unwrap_or(false);
            if !matched {
                continue;
            }
        }
        if !options.agents.is_empty() && !options.agents.iter().any(|a| a == &node.agent) {
            continue;
        }
        if node.size < options.min_size {
            continue;
        }

        let skip = |reason: String| -> SkippedItem {
            SkippedItem {
                path: node.path.clone(),
                bytes: node.size,
                rule_id: node.rule_id.clone(),
                reason,
            }
        };

        if !options.include_running && running.iter().any(|a| a == &node.agent) {
            skipped.push(skip(format!("{} 正在运行", node.agent)));
            continue;
        }
        if node.action == Action::Review && !options.include_review {
            skipped.push(skip("需人工确认（--include-review 才处理）".to_string()));
            continue;
        }

        items.push(PlanItem {
            index: 0,
            path: node.path.clone(),
            bytes: node.size,
            mtime: node.mtime,
            agent: node.agent.clone(),
            class: node.class,
            risk: node.risk.unwrap_or(Risk::High),
            action: node.action,
            rule_id: node.rule_id.clone(),
        });
    }

    // 大的先处理：中途失败时，收益最大的部分已经落袋
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
    for (i, item) in items.iter_mut().enumerate() {
        item.index = i + 1;
    }
    skipped.sort_by_key(|i| std::cmp::Reverse(i.bytes));

    Plan {
        generated_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        options: options.clone(),
        running_agents: running.to_vec(),
        items,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(path: &str, bytes: u64, class: DataClass, action: Action, agent: &str) -> Node {
        Node {
            path: PathBuf::from(path),
            size: bytes,
            mtime: None,
            agent: agent.to_string(),
            class,
            risk: Some(Risk::Low),
            action,
            rule_id: Some(format!("{}.rule", agent)),
            note: None,
        }
    }

    #[test]
    fn review_items_are_skipped_by_default() {
        let nodes = vec![
            node("C:/a", 100, DataClass::Ephemeral, Action::Clean, "demo"),
            node("C:/b", 200, DataClass::Product, Action::Review, "demo"),
        ];
        let plan = build(&nodes, &[], &PlanOptions::default());
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].path, PathBuf::from("C:/a"));
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].reason.contains("人工确认"));
    }

    #[test]
    fn review_items_included_when_opted_in() {
        let nodes = vec![node(
            "C:/b",
            200,
            DataClass::Product,
            Action::Review,
            "demo",
        )];
        let opts = PlanOptions {
            include_review: true,
            ..Default::default()
        };
        assert_eq!(build(&nodes, &[], &opts).items.len(), 1);
    }

    #[test]
    fn running_agent_is_skipped() {
        let nodes = vec![node(
            "C:/a",
            100,
            DataClass::Ephemeral,
            Action::Clean,
            "demo",
        )];
        let plan = build(&nodes, &["demo".to_string()], &PlanOptions::default());
        assert!(plan.items.is_empty());
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].reason.contains("正在运行"));
    }

    #[test]
    fn asset_and_unknown_never_enter_plan() {
        let nodes = vec![
            node("C:/a", 100, DataClass::Asset, Action::Protect, "demo"),
            node("C:/b", 100, DataClass::Unknown, Action::Review, "demo"),
        ];
        let opts = PlanOptions {
            include_review: true,
            include_running: true,
            ..Default::default()
        };
        let plan = build(&nodes, &[], &opts);
        assert!(plan.items.is_empty());
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn min_size_and_rule_filters_apply() {
        let nodes = vec![
            node("C:/a", 100, DataClass::Ephemeral, Action::Clean, "demo"),
            node("C:/b", 900, DataClass::Ephemeral, Action::Clean, "demo"),
        ];
        let opts = PlanOptions {
            min_size: 500,
            ..Default::default()
        };
        assert_eq!(build(&nodes, &[], &opts).items.len(), 1);
    }

    #[test]
    fn items_are_sorted_and_indexed() {
        let nodes = vec![
            node("C:/small", 10, DataClass::Ephemeral, Action::Clean, "demo"),
            node("C:/big", 1000, DataClass::Ephemeral, Action::Clean, "demo"),
        ];
        let plan = build(&nodes, &[], &PlanOptions::default());
        assert_eq!(plan.items[0].bytes, 1000);
        assert_eq!(plan.items[0].index, 1);
        assert_eq!(plan.items[1].index, 2);
    }
    #[test]
    fn retain_keeps_only_selected_indices() {
        let nodes = vec![
            node("C:/a", 100, DataClass::Ephemeral, Action::Clean, "demo"),
            node("C:/b", 200, DataClass::Ephemeral, Action::Clean, "demo"),
            node("C:/c", 300, DataClass::Ephemeral, Action::Clean, "demo"),
        ];
        let plan = build(&nodes, &[], &PlanOptions::default());
        assert_eq!(plan.items.len(), 3);

        let filtered = plan.retain(&[plan.items[1].index]);
        assert_eq!(filtered.items.len(), 1);
        assert_eq!(filtered.items[0].path, plan.items[1].path);
        assert_eq!(filtered.total_bytes(), plan.items[1].bytes);

        // 越界下标不应影响结果，也不应 panic
        assert!(plan.retain(&[9999]).is_empty());
    }
}
