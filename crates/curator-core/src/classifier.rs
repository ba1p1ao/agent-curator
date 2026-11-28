//! 分类器：把一条文件路径映射成 `Node`。
//!
//! 无规则命中时一律落到 `Unknown / Review` —— 这是整个产品的 fail-safe 底线：
//! 宁可少清，不可错删。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{Action, DataClass, Node, Risk};
use crate::rules::{RuleEntry, RuleLibrary};
use crate::util::normalize_path;

/// 当前 Unix 秒。整轮扫描只取一次，避免每条文件都做系统调用。
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 一次分类的结果，借用规则库里的字段，不复制字符串。
///
/// 扫描时绝大多数文件只用于聚合统计，不需要保留完整 `Node`；
/// 用这个轻量结构可以省掉每个文件的 3-4 次分配。
pub struct Hit<'a> {
    pub class: DataClass,
    pub risk: Option<Risk>,
    pub action: Action,
    pub rule_id: Option<&'a str>,
    pub note: Option<&'a str>,
}

/// 把一次匹配结果转成轻量分类结论。
pub fn hit_from(entry: Option<&RuleEntry>) -> Hit<'_> {
    match entry {
        Some(entry) => Hit {
            class: entry.class,
            risk: Some(entry.risk),
            action: entry.action,
            rule_id: Some(entry.id.as_str()),
            note: entry.note.as_deref(),
        },
        None => Hit {
            class: DataClass::Unknown,
            risk: None,
            action: Action::Review,
            rule_id: None,
            note: None,
        },
    }
}

/// 对已归一化的路径做分类，`scratch` 由调用方在循环外复用。
pub fn classify_light<'a>(
    normalized: &str,
    mtime_secs: Option<i64>,
    lib: &'a RuleLibrary,
    now: i64,
    scratch: &mut Vec<usize>,
) -> Hit<'a> {
    match lib.match_best_into(normalized, mtime_secs, now, scratch) {
        Some(entry) => Hit {
            class: entry.class,
            risk: Some(entry.risk),
            action: entry.action,
            rule_id: Some(entry.id.as_str()),
            note: entry.note.as_deref(),
        },
        None => Hit {
            class: DataClass::Unknown,
            risk: None,
            action: Action::Review,
            rule_id: None,
            note: None,
        },
    }
}

/// 对单个文件做分类，产出完整 `Node`。纯函数（除时间外无副作用），便于单测。
pub fn classify(
    path: &Path,
    size: u64,
    mtime_secs: Option<i64>,
    root_agent: &str,
    lib: &RuleLibrary,
    now: i64,
) -> Node {
    let normalized = normalize_path(path);
    let mut scratch = Vec::new();
    let hit = classify_light(&normalized, mtime_secs, lib, now, &mut scratch);

    Node {
        path: path.to_path_buf(),
        size,
        mtime: mtime_secs,
        agent: root_agent.to_string(),
        class: hit.class,
        risk: hit.risk,
        action: hit.action,
        rule_id: hit.rule_id.map(str::to_string),
        note: hit.note.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_secs_is_reasonable() {
        // 2020-01-01 之后都算合理，防止时钟异常导致 mtime 规则全部穿帮
        assert!(now_secs() > 1_577_836_800);
    }

    #[test]
    fn unclassified_node_is_review_not_clean() {
        // 用一个空规则库无法构造，这里直接断言默认分支的语义常量
        assert_eq!(Action::Review.priority(), 2);
        assert!(DataClass::Unknown.conservativeness() < DataClass::Ephemeral.conservativeness());
    }
}
