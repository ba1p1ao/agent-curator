//! 只读扫描器。
//!
//! 安全约束（本文件内不存在任何写操作，请不要在这里引入 `fs::remove_*`）：
//! - 不跟随符号链接 / 目录联接，防止体积重复计算与越界枚举
//! - 只读元数据（size / mtime），不读文件内容，不做 hash
//! - 单个文件出错只累加错误计数，不中断整轮扫描

use std::path::PathBuf;
use std::time::{Instant, UNIX_EPOCH};

use jwalk::WalkDirGeneric;

use crate::aggregate::Aggregates;
use crate::classifier::{hit_from, now_secs};
use crate::model::{DataClass, Node};
use crate::rules::RuleLibrary;

/// 并行阶段读好的文件元数据：(体积, mtime 秒)
type EntryMeta = (u64, Option<i64>);
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
    /// 仅在 `ScanOptions::collect_candidates` 为真时填充
    pub candidates: Vec<Node>,
}

/// 扫描行为开关。默认不保留节点，避免百万文件时内存爆炸。
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanOptions {
    /// 采集可清理候选（ephemeral / product / workspace）
    pub collect_candidates: bool,
    /// 采集资产条目（备份用）
    pub collect_assets: bool,
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
pub fn scan_with<F>(targets: &[ScanTarget], lib: &RuleLibrary, on_event: F) -> ScanOutcome
where
    F: FnMut(ScanEvent<'_>),
{
    scan_with_opts(targets, lib, &ScanOptions::default(), on_event)
}

/// 带进度回调与采集开关的扫描。
pub fn scan_with_opts<F>(
    targets: &[ScanTarget],
    lib: &RuleLibrary,
    opts: &ScanOptions,
    mut on_event: F,
) -> ScanOutcome
where
    F: FnMut(ScanEvent<'_>),
{
    let started = Instant::now();
    let now = now_secs();
    let mut agg = Aggregates::default();
    let mut roots: Vec<RootStat> = Vec::with_capacity(targets.len());
    let mut candidates: Vec<Node> = Vec::new();
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

        // 关键：把 stat 挪到并行 worker 里。
        //
        // jwalk 的 DirEntry::metadata() 每次调用都会重新 symlink_metadata，
        // 而且调用点在消费线程上 —— 十万个文件就是十万次串行 syscall，
        // 实测占了扫描总时间的一半以上。process_read_dir 在 rayon 线程池里执行，
        // 在这里把体积和 mtime 读出来塞进 client_state，主循环就只剩纯计算。
        let walker = WalkDirGeneric::<((), Option<EntryMeta>)>::new(&target.path)
            .skip_hidden(false)
            .follow_links(false)
            .process_read_dir(|_depth, _path, _state, children| {
                for child in children.iter_mut().flatten() {
                    if child.file_type().is_dir() {
                        continue;
                    }
                    child.client_state = child.metadata().ok().map(|md| {
                        let mtime = md
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64);
                        (md.len(), mtime)
                    });
                }
            });

        // 循环外复用的匹配缓冲，避免每个文件都分配一个 Vec
        let mut scratch: Vec<usize> = Vec::with_capacity(8);

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

            // 体积与 mtime 已在并行阶段读好；None 表示那个文件的 stat 失败了
            let (size, mtime) = match entry.client_state {
                Some(meta) => meta,
                None => {
                    errors += 1;
                    continue;
                }
            };

            let path = entry.path();
            let normalized = normalize_path(&path);
            let hit = hit_from(lib.match_best_into(&normalized, mtime, now, &mut scratch));
            agg.add_hit(&target.agent, size, &hit, &path);

            // 只有真要保留的条目才构造完整 Node（含 4 处字符串复制）
            let wanted = (opts.collect_candidates
                && matches!(
                    hit.class,
                    DataClass::Ephemeral | DataClass::Product | DataClass::Workspace
                ))
                || (opts.collect_assets && hit.class == DataClass::Asset);
            if wanted {
                candidates.push(Node {
                    path: path.to_path_buf(),
                    size,
                    mtime,
                    agent: target.agent.clone(),
                    class: hit.class,
                    risk: hit.risk,
                    action: hit.action,
                    rule_id: hit.rule_id.map(str::to_string),
                    note: hit.note.map(str::to_string),
                });
            }

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
        candidates,
    }
}
