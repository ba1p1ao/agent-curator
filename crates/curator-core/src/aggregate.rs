//! 流式聚合：扫描过程中直接累加，不保留全量节点，避免百万级文件把内存吃穿。

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::model::{Action, DataClass, Node, Risk};

#[derive(Debug, Default, Clone, Copy)]
pub struct Bucket {
    pub files: u64,
    pub bytes: u64,
}

impl Bucket {
    pub fn add(&mut self, size: u64) {
        self.files += 1;
        self.bytes += size;
    }
}

#[derive(Debug, Clone)]
pub struct RuleAgg {
    pub rule_id: String,
    pub agent: String,
    pub class: DataClass,
    pub risk: Risk,
    pub action: Action,
    pub note: Option<String>,
    pub files: u64,
    pub bytes: u64,
    pub sample: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct LargestItem {
    pub path: PathBuf,
    pub bytes: u64,
    pub class: DataClass,
    pub rule_id: Option<String>,
}

const LARGEST_CAP: usize = 20;

#[derive(Debug, Default)]
pub struct Aggregates {
    pub total_files: u64,
    pub total_bytes: u64,
    pub by_agent: BTreeMap<String, Bucket>,
    pub by_class: BTreeMap<DataClass, Bucket>,
    pub by_rule: HashMap<String, RuleAgg>,
    /// 低风险可重建：可直接释放
    pub safe_bytes: u64,
    /// 需要人确认：高风险垃圾 + 用户数据 + 项目产物
    pub confirm_bytes: u64,
    /// 受保护：资产 + 未识别
    pub protected_bytes: u64,
    pub largest: Vec<LargestItem>,
    /// 未识别样本（有上限），用于规则迭代排查
    pub unmatched: Vec<LargestItem>,
    pub unmatched_truncated: bool,
}

/// 未识别样本保留条数上限，避免规则覆盖率低时把内存吃穿
const UNMATCHED_CAP: usize = 2000;

impl Aggregates {
    /// 热路径：直接吃分类结果，避免为每个文件构造完整 `Node`。
    ///
    /// 关键点是**命中时不 clone key**：老实现每次 `entry(key.clone())` 都会分配，
    /// 十万级文件下这类分配比匹配本身还贵。
    #[allow(clippy::too_many_arguments)]
    pub fn add_hit(
        &mut self,
        agent: &str,
        size: u64,
        hit: &crate::classifier::Hit<'_>,
        path: &std::path::Path,
    ) {
        self.total_files += 1;
        self.total_bytes += size;

        if let Some(bucket) = self.by_agent.get_mut(agent) {
            bucket.add(size);
        } else {
            let mut bucket = Bucket::default();
            bucket.add(size);
            self.by_agent.insert(agent.to_string(), bucket);
        }
        self.by_class.entry(hit.class).or_default().add(size);

        let key = hit.rule_id.unwrap_or("unmatched");
        let risk = hit.risk.unwrap_or(Risk::High);
        if let Some(agg) = self.by_rule.get_mut(key) {
            agg.files += 1;
            agg.bytes += size;
        } else {
            self.by_rule.insert(
                key.to_string(),
                RuleAgg {
                    rule_id: key.to_string(),
                    agent: agent.to_string(),
                    class: hit.class,
                    risk,
                    action: hit.action,
                    note: hit.note.map(str::to_string),
                    files: 1,
                    bytes: size,
                    sample: Some(path.to_path_buf()),
                },
            );
        }

        match hit.class {
            DataClass::Asset | DataClass::Unknown => self.protected_bytes += size,
            DataClass::Ephemeral => {
                // 未知风险按高处理，宁可让人看一眼
                if hit.risk.unwrap_or(Risk::High) <= Risk::Medium {
                    self.safe_bytes += size;
                } else {
                    self.confirm_bytes += size;
                }
            }
            // 用户数据与项目产物都不能一键释放，必须过人工
            DataClass::Workspace | DataClass::Product => self.confirm_bytes += size,
        }

        if hit.class == DataClass::Unknown {
            if self.unmatched.len() < UNMATCHED_CAP {
                self.unmatched.push(LargestItem {
                    path: path.to_path_buf(),
                    bytes: size,
                    class: hit.class,
                    rule_id: None,
                });
            } else {
                self.unmatched_truncated = true;
            }
        }

        if matches!(hit.class, DataClass::Ephemeral | DataClass::Product) {
            self.push_largest_hit(path, size, hit.class, hit.rule_id);
        }
    }

    /// 完整 `Node` 入口，保留给调用方与测试。
    pub fn add(&mut self, node: &Node) {
        let hit = crate::classifier::Hit {
            class: node.class,
            risk: node.risk,
            action: node.action,
            rule_id: node.rule_id.as_deref(),
            note: node.note.as_deref(),
        };
        self.add_hit(&node.agent, node.size, &hit, &node.path);
    }

    /// 先比体积再复制路径：榜单满之后，绝大多数文件都在这里被挡掉，
    /// 老实现会先 clone 一个 PathBuf 才判断要不要留。
    fn push_largest_hit(
        &mut self,
        path: &std::path::Path,
        bytes: u64,
        class: DataClass,
        rule_id: Option<&str>,
    ) {
        if self.largest.len() >= LARGEST_CAP {
            let smallest = self.largest.last().map(|i| i.bytes).unwrap_or(0);
            if bytes <= smallest {
                return;
            }
        }

        self.largest.push(LargestItem {
            path: path.to_path_buf(),
            bytes,
            class,
            rule_id: rule_id.map(str::to_string),
        });
        self.largest.sort_by_key(|i| std::cmp::Reverse(i.bytes));
        self.largest.truncate(LARGEST_CAP);
    }

    /// 按体积倒序的规则聚合结果，供报表取 Top N。
    pub fn rules_by_size(&self) -> Vec<&RuleAgg> {
        let mut list: Vec<&RuleAgg> = self.by_rule.values().collect();
        list.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.rule_id.cmp(&b.rule_id)));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(class: DataClass, risk: Option<Risk>, size: u64) -> Node {
        Node {
            path: PathBuf::from("C:/x/y.bin"),
            size,
            mtime: None,
            agent: "demo".to_string(),
            class,
            risk,
            action: Action::Clean,
            rule_id: Some("demo.x".to_string()),
            note: None,
        }
    }

    #[test]
    fn ephemeral_low_risk_counts_as_safe() {
        let mut agg = Aggregates::default();
        agg.add(&node(DataClass::Ephemeral, Some(Risk::Low), 100));
        assert_eq!(agg.safe_bytes, 100);
        assert_eq!(agg.confirm_bytes, 0);
    }

    #[test]
    fn ephemeral_high_risk_requires_confirmation() {
        let mut agg = Aggregates::default();
        agg.add(&node(DataClass::Ephemeral, Some(Risk::High), 100));
        assert_eq!(agg.safe_bytes, 0);
        assert_eq!(agg.confirm_bytes, 100);
    }

    #[test]
    fn asset_and_unknown_are_protected() {
        let mut agg = Aggregates::default();
        agg.add(&node(DataClass::Asset, Some(Risk::Critical), 10));
        agg.add(&node(DataClass::Unknown, None, 20));
        assert_eq!(agg.protected_bytes, 30);
        assert_eq!(agg.safe_bytes, 0);
        assert_eq!(agg.confirm_bytes, 0);
    }
}
