//! 隔离区：软删除的唯一实现。
//!
//! 三条铁律：
//! 1. **绝不直接删**。同卷用 `rename`（原子）；跨卷用「复制 → 校验字节数 → 删源」。
//! 2. **先落 manifest 骨架，再逐项 append journal**。中途崩溃时文件不会变成孤儿。
//! 3. **还原不覆盖**。目标已存在就记为冲突并跳过，绝不静默覆盖用户文件。

use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::model::{DataClass, Risk};
use crate::plan::Plan;

/// 隔离区默认目录名，挂在用户主目录下
pub const DEFAULT_BIN_DIR: &str = ".curator-bin";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    /// 正在执行，manifest 可能不完整
    Applying,
    /// 全部成功移入隔离区
    Applied,
    /// 部分成功
    Partial,
    /// 已还原
    Restored,
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            RunStatus::Applying => "执行中",
            RunStatus::Applied => "已隔离",
            RunStatus::Partial => "部分成功",
            RunStatus::Restored => "已还原",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MoveMode {
    /// 同卷改名，原子且保留 mtime
    Rename,
    /// 跨卷复制后删源
    Copied,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunItem {
    pub index: usize,
    pub original: PathBuf,
    pub stored: PathBuf,
    pub bytes: u64,
    pub mtime: Option<i64>,
    pub agent: String,
    pub class: DataClass,
    pub risk: Risk,
    pub rule_id: Option<String>,
    pub mode: MoveMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunManifest {
    pub run_id: String,
    pub created_at: String,
    pub status: RunStatus,
    pub bin_dir: PathBuf,
    pub planned_items: usize,
    pub total_bytes: u64,
    pub items: Vec<RunItem>,
    /// 未能移入隔离区的条目（源文件未改动），用于事后审计
    #[serde(default)]
    pub failures: Vec<RunFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunFailure {
    pub original: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct RunSummary {
    pub run_id: String,
    pub created_at: String,
    pub status: RunStatus,
    pub items: usize,
    pub bytes: u64,
}

#[derive(Debug, Default)]
pub struct ApplyReport {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub moved: usize,
    pub bytes: u64,
    pub failures: Vec<(PathBuf, String)>,
}

#[derive(Debug, Default)]
pub struct RestoreReport {
    pub run_id: String,
    pub restored: usize,
    pub restored_bytes: u64,
    pub conflicts: Vec<(PathBuf, String)>,
    pub failures: Vec<(PathBuf, String)>,
}

pub struct Quarantine {
    root: PathBuf,
}

impl Quarantine {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    /// 把计划里的条目移入隔离区。`plan` 为空时直接报错，避免生成空批次。
    pub fn apply(&self, plan: &Plan) -> Result<ApplyReport> {
        if plan.is_empty() {
            bail!("计划为空，没有可执行的内容");
        }

        // 防御：隔离区不能落在待清理路径内部，否则会自我吞噬
        for item in &plan.items {
            if item.path.starts_with(&self.root) {
                bail!(
                    "待清理路径位于隔离区内部，已中止：{}",
                    item.path.display()
                );
            }
        }

        let run_id = new_run_id();
        let run_dir = self.runs_dir().join(&run_id);
        let files_dir = run_dir.join("files");
        fs::create_dir_all(&files_dir)
            .with_context(|| format!("创建隔离区失败: {}", files_dir.display()))?;

        let mut manifest = RunManifest {
            run_id: run_id.clone(),
            created_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            status: RunStatus::Applying,
            bin_dir: self.root.clone(),
            planned_items: plan.items.len(),
            total_bytes: plan.total_bytes(),
            items: Vec::new(),
            failures: Vec::new(),
        };
        // 先把骨架落盘：即使下一秒崩溃，也能从 journal 恢复出真实状态
        write_json(&run_dir.join("manifest.json"), &manifest)?;

        let journal_path = run_dir.join("journal.jsonl");
        let mut journal = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(ext(&journal_path))
                .with_context(|| format!("无法写入 journal: {}", journal_path.display()))?,
        );

        let mut report = ApplyReport {
            run_id: run_id.clone(),
            run_dir: run_dir.clone(),
            ..Default::default()
        };

        for item in &plan.items {
            let slot = files_dir.join(format!("{:05}", item.index));
            if let Err(e) = fs::create_dir_all(ext(&slot)) {
                report.failures.push((item.path.clone(), e.to_string()));
                continue;
            }

            let name = item
                .path
                .file_name()
                .map(|n| n.to_os_string())
                .unwrap_or_else(|| "unnamed".into());
            let stored = slot.join(name);

            // 符号链接一律拒绝：跟随它会删掉真实目标
            if let Ok(meta) = fs::symlink_metadata(ext(&item.path)) {
                if meta.file_type().is_symlink() {
                    report
                        .failures
                        .push((item.path.clone(), "符号链接，已拒绝".to_string()));
                    continue;
                }
            }

            match move_file(&item.path, &stored, item.mtime) {
                Ok(mode) => {
                    let record = RunItem {
                        index: item.index,
                        original: item.path.clone(),
                        stored: stored.clone(),
                        bytes: item.bytes,
                        mtime: item.mtime,
                        agent: item.agent.clone(),
                        class: item.class,
                        risk: item.risk,
                        rule_id: item.rule_id.clone(),
                        mode,
                    };
                    if let Ok(line) = serde_json::to_string(&record) {
                        let _ = writeln!(journal, "{}", line);
                        let _ = journal.flush();
                    }
                    report.moved += 1;
                    report.bytes += item.bytes;
                    manifest.items.push(record);
                }
                Err(e) => {
                    report.failures.push((item.path.clone(), e.to_string()));
                }
            }
        }

        manifest.failures = report
            .failures
            .iter()
            .map(|(path, reason)| RunFailure {
                original: path.clone(),
                reason: reason.clone(),
            })
            .collect();

        manifest.status = if report.failures.is_empty() {
            RunStatus::Applied
        } else if report.moved == 0 {
            RunStatus::Applying
        } else {
            RunStatus::Partial
        };
        write_json(&run_dir.join("manifest.json"), &manifest)?;

        Ok(report)
    }

    pub fn list_runs(&self) -> Result<Vec<RunSummary>> {
        let dir = self.runs_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut runs = Vec::new();
        for entry in fs::read_dir(ext(&dir))? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let manifest_path = entry.path().join("manifest.json");
            let Ok(manifest) = read_json::<RunManifest>(&manifest_path) else {
                continue;
            };
            runs.push(RunSummary {
                run_id: manifest.run_id.clone(),
                created_at: manifest.created_at.clone(),
                status: manifest.status,
                items: manifest.items.len(),
                bytes: manifest.items.iter().map(|i| i.bytes).sum(),
            });
        }
        // run_id 里带时间戳，字典序即时间序
        runs.sort_by(|a, b| b.run_id.cmp(&a.run_id));
        Ok(runs)
    }

    pub fn load_manifest(&self, run_id: &str) -> Result<RunManifest> {
        let path = self.runs_dir().join(run_id).join("manifest.json");
        read_json(&path).with_context(|| format!("找不到批次 {} 的 manifest", run_id))
    }

    /// 还原一个批次。倒序处理，让嵌套目录先于父目录回来。
    pub fn restore(&self, run_id: &str) -> Result<RestoreReport> {
        let run_dir = self.runs_dir().join(run_id);
        let mut manifest = self.load_manifest(run_id)?;

        if manifest.status == RunStatus::Restored {
            bail!("批次 {} 已经还原过了", run_id);
        }

        let mut report = RestoreReport {
            run_id: run_id.to_string(),
            ..Default::default()
        };

        let mut items = manifest.items.clone();
        items.sort_by_key(|i| std::cmp::Reverse(i.index));

        for item in &items {
            if !item.stored.exists() {
                report
                    .conflicts
                    .push((item.original.clone(), "隔离区中已不存在该文件".to_string()));
                continue;
            }
            if item.original.exists() {
                report
                    .conflicts
                    .push((item.original.clone(), "原路径已有同名文件，未覆盖".to_string()));
                continue;
            }
            if let Some(parent) = item.original.parent() {
                if let Err(e) = fs::create_dir_all(ext(parent)) {
                    report
                        .failures
                        .push((item.original.clone(), format!("重建目录失败: {}", e)));
                    continue;
                }
            }
            match move_file(&item.stored, &item.original, item.mtime) {
                Ok(_) => {
                    report.restored += 1;
                    report.restored_bytes += item.bytes;
                }
                Err(e) => report.failures.push((item.original.clone(), e.to_string())),
            }
        }

        manifest.status = if report.failures.is_empty() && report.conflicts.is_empty() {
            RunStatus::Restored
        } else {
            RunStatus::Partial
        };
        write_json(&run_dir.join("manifest.json"), &manifest)?;

        // 只有全部归位才删掉空壳。
        // 存在冲突时绝不动 files/ —— 那里还躺着没还原成功的文件，
        // 删掉就等于把用户唯一的一份数据抹了。
        if report.restored == manifest.items.len() {
            let _ = fs::remove_dir_all(ext(&run_dir.join("files")));
        }
        Ok(report)
    }

    /// 保留最新的 `keep` 个批次，其余连同文件一起物理删除。
    pub fn purge(&self, keep: usize) -> Result<Vec<String>> {
        let runs = self.list_runs()?;
        let mut removed = Vec::new();
        for run in runs.into_iter().skip(keep) {
            let dir = self.runs_dir().join(&run.run_id);
            fs::remove_dir_all(ext(&dir))
                .with_context(|| format!("删除批次失败: {}", dir.display()))?;
            removed.push(run.run_id);
        }
        Ok(removed)
    }

    /// 隔离区当前占用
    pub fn usage(&self) -> (usize, u64) {
        let runs = self.list_runs().unwrap_or_default();
        (runs.len(), runs.iter().map(|r| r.bytes).sum())
    }
}

impl Default for Quarantine {
    fn default() -> Self {
        Self::new(PathBuf::from(DEFAULT_BIN_DIR))
    }
}

fn new_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "{}-{:04x}",
        Local::now().format("%Y%m%d-%H%M%S"),
        nanos & 0xffff
    )
}

/// 把文件搬到目标位置：先试 rename，失败再走复制校验。
fn move_file(src: &Path, dst: &Path, mtime: Option<i64>) -> Result<MoveMode> {
    if fs::rename(ext(src), ext(dst)).is_ok() {
        return Ok(MoveMode::Rename);
    }

    let expected = fs::metadata(ext(src))
        .with_context(|| format!("读取源文件失败: {}", src.display()))?
        .len();

    fs::copy(ext(src), ext(dst))
        .with_context(|| format!("复制失败: {} -> {}", src.display(), dst.display()))?;

    let actual = fs::metadata(ext(dst))?.len();
    if actual != expected {
        let _ = fs::remove_file(ext(dst));
        bail!("复制校验失败：源 {} 字节 / 目标 {} 字节", expected, actual);
    }

    if let Some(secs) = mtime {
        // 跨卷复制会丢修改时间，这里补回来，保证原有 "N 天未访问" 规则仍然成立
        if let Ok(stamp) = u64::try_from(secs) {
            let modified = UNIX_EPOCH + std::time::Duration::from_secs(stamp);
            let _ = fs::set_times(ext(dst), fs::FileTimes::new().set_modified(modified));
        }
    }

    // 只有确认副本完整，才允许删源
    if let Err(e) = fs::remove_file(ext(src)) {
        let _ = fs::remove_file(ext(dst));
        bail!("副本已完整但删除源文件失败（已回滚副本）: {}", e);
    }
    Ok(MoveMode::Copied)
}

/// Windows 长路径支持：超过 240 字符时加 `\\?\` 前缀。
#[cfg(windows)]
fn ext(path: &Path) -> PathBuf {
    let s = path.as_os_str().to_string_lossy().to_string();
    if s.starts_with(r"\\?\") || s.len() < 240 || !path.is_absolute() {
        return path.to_path_buf();
    }
    if let Some(rest) = s.strip_prefix(r"\\") {
        return PathBuf::from(format!(r"\\?\UNC\{}", rest));
    }
    PathBuf::from(format!(r"\\?\{}", s))
}

#[cfg(not(windows))]
fn ext(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    // 先写临时文件再改名：manifest 是还原的唯一依据，不能出现半截 JSON
    let tmp = path.with_extension("json.tmp");
    fs::write(ext(&tmp), text).with_context(|| format!("写入失败: {}", tmp.display()))?;
    fs::rename(ext(&tmp), ext(path))
        .with_context(|| format!("提交失败: {}", path.display()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(ext(path))
        .with_context(|| format!("读取失败: {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Action;
    use crate::plan::{Plan, PlanItem, PlanOptions};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "curator-test-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plan_of(paths: &[&Path], bytes: u64) -> Plan {
        let items = paths
            .iter()
            .enumerate()
            .map(|(i, p)| PlanItem {
                index: i + 1,
                path: p.to_path_buf(),
                bytes,
                mtime: None,
                agent: "demo".into(),
                class: DataClass::Ephemeral,
                risk: Risk::Low,
                action: Action::Clean,
                rule_id: Some("demo.cache".into()),
            })
            .collect();
        Plan {
            generated_at: "test".into(),
            options: PlanOptions::default(),
            running_agents: Vec::new(),
            items,
            skipped: Vec::new(),
        }
    }

    #[test]
    fn apply_then_restore_round_trip() {
        let work = temp_dir("roundtrip");
        let src_dir = work.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        let a = src_dir.join("a.log");
        let b = src_dir.join("b.log");
        fs::write(&a, b"hello").unwrap();
        fs::write(&b, b"world!!").unwrap();

        let bin = work.join("bin");
        let q = Quarantine::new(bin);
        let plan = plan_of(&[&a, &b], 5);
        let report = q.apply(&plan).unwrap();

        assert_eq!(report.moved, 2);
        assert!(!a.exists(), "源文件应已被移走");
        assert!(!b.exists());
        assert_eq!(q.list_runs().unwrap().len(), 1);

        let restored = q.restore(&report.run_id).unwrap();
        assert_eq!(restored.restored, 2);
        assert_eq!(fs::read(&a).unwrap(), b"hello");
        assert_eq!(fs::read(&b).unwrap(), b"world!!");
    }

    #[test]
    fn restore_never_overwrites_existing_file() {
        let work = temp_dir("conflict");
        let src_dir = work.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        let a = src_dir.join("a.log");
        fs::write(&a, b"original").unwrap();

        let q = Quarantine::new(work.join("bin"));
        let report = q.apply(&plan_of(&[&a], 8)).unwrap();

        // 还原前原路径被别的东西占了
        fs::write(&a, b"someone else").unwrap();
        let restored = q.restore(&report.run_id).unwrap();

        assert_eq!(restored.restored, 0);
        assert_eq!(restored.conflicts.len(), 1);
        assert_eq!(fs::read(&a).unwrap(), b"someone else");

        // 关键：冲突的文件必须still 留在隔离区，不能被清理掉
        let manifest = q.load_manifest(&report.run_id).unwrap();
        assert!(
            manifest.items[0].stored.exists(),
            "还原冲突后，隔离区里的副本不能被删除"
        );
    }

    #[test]
    fn refuses_paths_inside_quarantine() {
        let work = temp_dir("nested");
        let q = Quarantine::new(work.join("bin"));
        let inside = work.join("bin").join("x.log");
        fs::create_dir_all(inside.parent().unwrap()).unwrap();
        fs::write(&inside, b"x").unwrap();

        let err = q.apply(&plan_of(&[&inside], 1)).unwrap_err();
        assert!(err.to_string().contains("隔离区内部"));
    }

    #[test]
    fn purge_keeps_newest_runs() {
        let work = temp_dir("purge");
        let src_dir = work.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let q = Quarantine::new(work.join("bin"));
        for i in 0..3 {
            let f = src_dir.join(format!("f{}.log", i));
            fs::write(&f, b"data").unwrap();
            q.apply(&plan_of(&[&f], 4)).unwrap();
        }
        assert_eq!(q.list_runs().unwrap().len(), 3);

        let removed = q.purge(1).unwrap();
        assert_eq!(removed.len(), 2);
        assert_eq!(q.list_runs().unwrap().len(), 1);
    }
}
