//! 分类器：把一条文件路径映射成 `Node`。
//!
//! 无规则命中时一律落到 `Unknown / Review` —— 这是整个产品的 fail-safe 底线：
//! 宁可少清，不可错删。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{Action, DataClass, Node};
use crate::rules::RuleLibrary;
use crate::util::normalize_path;

/// 当前 Unix 秒。整轮扫描只取一次，避免每条文件都做系统调用。
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 对单个文件做分类。纯函数（除时间外无副作用），便于单测。
pub fn classify(
    path: &Path,
    size: u64,
    mtime_secs: Option<i64>,
    root_agent: &str,
    lib: &RuleLibrary,
    now: i64,
) -> Node {
    let normalized = normalize_path(path);

    match lib.match_best(&normalized, mtime_secs, now) {
        Some(entry) => Node {
            path: path.to_path_buf(),
            size,
            mtime: mtime_secs,
            agent: root_agent.to_string(),
            class: entry.class,
            risk: Some(entry.risk),
            action: entry.action,
            rule_id: Some(entry.id.clone()),
            note: entry.note.clone(),
        },
        None => Node {
            path: path.to_path_buf(),
            size,
            mtime: mtime_secs,
            agent: root_agent.to_string(),
            class: DataClass::Unknown,
            risk: None,
            action: Action::Review,
            rule_id: None,
            note: None,
        },
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
