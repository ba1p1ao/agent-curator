//! 只读扫描器。
//!
//! 安全约束（本文件内不存在任何写操作，请不要在这里引入 `fs::remove_*`）：
//! - 不跟随符号链接 / 目录联接，防止体积重复计算与越界枚举
//! - 只读元数据（size / mtime），不读文件内容，不做 hash
//! - 单个文件出错只累加错误计数，不中断整轮扫描

use std::path::PathBuf;
use std::time::{Instant, UNIX_EPOCH};

use jwalk::WalkDir;

use crate::aggregate::Aggregates;
use crate::classifier::{classify, now_secs};
use crate::rules::RuleLibrary;
use crate::util::normalize_path;

/// 一个待扫描的根目录。
#[derive(Debug, Clone)]
pub struct ScanTarget {
    pub agent: String,
    pub root_id: String,
    pub path: PathBuf,
}

/// 单个根目录的扫描结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RootStat {
    pub agent: String,
    pub root_id: String,
    pub path: String,
    pub files: u64,
    pub bytes: u64,
    pub errors: u64,
}

#[derive(Debug)]
pub struct ScanOutcome {
    pub agg: Aggregates,
    pub roots: Vec<RootStat>,
    pub elapsed_ms: u128,
    pub total_files: u64,
    pub total_bytes: u64,
}

/// 扫描进度事件，供 CLI 输出到 stderr。
#[derive(Debug)]
pub enum ScanEvent<'a> {
    RootStarted {
        index: usize,
        total: usize,
        target: &'a ScanTarget,
    },
    RootFinished {
        stat: &'a RootStat,
        elapsed_ms: u128,
    },
}

/// 根据规则库里的 roots 定义自动发现本机已安装的 agent。
///
/// `probe` 全部不存在 → 判定为未安装，跳过。
pub fn discover(lib: &RuleLibrary) -> Vec<ScanTarget> {
    lib.roots
        .iter()
        .filter_map(|root| {
            if !root.path.exists() {
                return None;
            }
            if !root.probe.is_empty() && !root.probe.iter().any(|p| root.path.join(p).exists()) {
                return None;
            }
            Some(ScanTarget {
                agent: root.agent.clone(),
                root_id: root.id.clone(),
                path: root.path.clone(),
            })
        })
        .collect()
}

/// 扫描全部目标根目录。
pub fn scan(targets: &[ScanTarget], lib: &RuleLibrary) -> ScanOutcome {
    scan_with(targets, lib, |_| {})
}

/// 带进度回调的扫描。
pub fn scan_with<F>(targets: &[ScanTarget], lib: &RuleLibrary, mut on_event: F) -> ScanOutcome
where
    F: FnMut(ScanEvent<'_>),
{
    let started = Instant::now();
    let now = now_secs();
    let mut agg = Aggregates::default();
    let mut roots: Vec<RootStat> = Vec::with_capacity(targets.len());
    let total = targets.len();

    for (index, target) in targets.iter().enumerate() {
        on_event(ScanEvent::RootStarted {
            index,
            total,
            target,
        });
        let root_started = Instant::now();

        let mut files = 0u64;
        let mut bytes = 0u64;
        let mut errors = 0u64;

        let walker = WalkDir::new(&target.path)
            .skip_hidden(false)
            .follow_links(false);

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    errors += 1;
                    continue;
                }
            };

            let file_type = entry.file_type();
            if file_type.is_dir() || file_type.is_symlink() {
                continue;
            }

            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => {
                    errors += 1;
                    continue;
                }
            };

            let size = metadata.len();
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);

            let path = entry.path();
            let node = classify(&path, size, mtime, &target.agent, lib, now);
            agg.add(&node);

            files += 1;
            bytes += size;
        }

        let stat = RootStat {
            agent: target.agent.clone(),
            root_id: target.root_id.clone(),
            path: normalize_path(&target.path),
            files,
            bytes,
            errors,
        };
        on_event(ScanEvent::RootFinished {
            stat: &stat,
            elapsed_ms: root_started.elapsed().as_millis(),
        });
        roots.push(stat);
    }

    let total_files = roots.iter().map(|r| r.files).sum();
    let total_bytes = roots.iter().map(|r| r.bytes).sum();

    ScanOutcome {
        agg,
        roots,
        elapsed_ms: started.elapsed().as_millis(),
        total_files,
        total_bytes,
    }
}
