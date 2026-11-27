//! AgentCurator 桌面壳。
//!
//! UI 只负责展示与确认，所有判断与文件操作都复用 `curator-core`：
//! 桌面端和 CLI 走的是同一套规则、同一个计划器、同一个隔离区。
//! 这样"GUI 删了东西 CLI 还原不了"这种分裂情况从架构上就不存在。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// 拦住"能编译、但一打开就显示 ERR_CONNECTION_REFUSED"的包：
// 没有 custom-protocol 时 tauri-build 会打上 cfg(dev)，运行时不加载内嵌前端，
// 而是去连 http://localhost:1420。release 构建出现这种情况必然是构建命令写错了。
#[cfg(all(dev, not(debug_assertions)))]
compile_error!(
    "release 构建必须启用 custom-protocol 特性。\n\
     请用 `pnpm tauri build`（会自动加），或手工 `cargo build --release -p curator-desktop \
     --features custom-protocol`。\n\
     否则程序不会内嵌前端，用户打开只会看到「localhost 拒绝连接」。"
);

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use curator_core::backup::{self, BackupManifest};
use curator_core::model::DataClass;
use curator_core::plan::{self, Plan, PlanOptions};
use curator_core::procs;
use curator_core::quarantine::{Quarantine, RunStatus, DEFAULT_BIN_DIR};
use curator_core::rules::{RuleLibrary, RuleSource};
use curator_core::scanner::{self, ScanOptions, ScanTarget};
use curator_core::util::home_dir;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- DTO

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BucketDto {
    key: String,
    label: String,
    files: u64,
    bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RootDto {
    agent: String,
    path: String,
    files: u64,
    bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanDto {
    total_files: u64,
    total_bytes: u64,
    elapsed_ms: u64,
    safe_bytes: u64,
    confirm_bytes: u64,
    protected_bytes: u64,
    agents: Vec<BucketDto>,
    classes: Vec<BucketDto>,
    roots: Vec<RootDto>,
    rule_count: usize,
    rule_source: String,
    running: Vec<String>,
    generated_at: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CleanOptions {
    #[serde(default)]
    include_review: bool,
    #[serde(default)]
    include_running: bool,
    #[serde(default)]
    min_size: u64,
    #[serde(default)]
    agents: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplyDto {
    run_id: String,
    moved: u64,
    bytes: u64,
    failures: Vec<(String, String)>,
    run_dir: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunDto {
    run_id: String,
    created_at: String,
    status: String,
    items: u64,
    bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoreDto {
    restored: u64,
    bytes: u64,
    conflicts: Vec<(String, String)>,
    failures: Vec<(String, String)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupInfoDto {
    name: String,
    path: String,
    size_bytes: u64,
    created_at: Option<String>,
    entries: Option<usize>,
    valid: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupDto {
    path: String,
    entries: u64,
    source_bytes: u64,
    zip_bytes: u64,
    sha256: String,
    failures: Vec<(String, String)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyDto {
    entries: usize,
    ok: usize,
    mismatched: Vec<(String, String)>,
    missing: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageDto {
    runs: usize,
    bytes: u64,
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RulesInfoDto {
    count: usize,
    agents: Vec<(String, String)>,
    roots: usize,
}

// ---------------------------------------------------------------- 路径

/// 外部 rules/ 优先，找不到就用编译进二进制的内置规则。
/// 这样用户把 exe 挪到任何位置都不会因为缺规则目录而瘫掉。
fn rules_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(env) = std::env::var_os("CURATOR_RULES_DIR") {
        candidates.push(PathBuf::from(env));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("rules"));
        candidates.push(cwd.join("../rules"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("rules"));
            candidates.push(dir.join("../rules"));
            candidates.push(dir.join("../../../rules"));
        }
    }
    candidates
}

fn bin_dir() -> PathBuf {
    if let Some(env) = std::env::var_os("CURATOR_BIN_DIR") {
        return PathBuf::from(env);
    }
    home_dir().join(DEFAULT_BIN_DIR)
}

fn backup_dir() -> PathBuf {
    if let Some(env) = std::env::var_os("CURATOR_BACKUP_DIR") {
        return PathBuf::from(env);
    }
    home_dir().join("curator-backups")
}

fn class_key(class: DataClass) -> &'static str {
    match class {
        DataClass::Asset => "asset",
        DataClass::Workspace => "workspace",
        DataClass::Product => "product",
        DataClass::Ephemeral => "ephemeral",
        DataClass::Unknown => "unknown",
    }
}

fn status_key(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Applying => "applying",
        RunStatus::Applied => "applied",
        RunStatus::Partial => "partial",
        RunStatus::Restored => "restored",
    }
}

fn load_library() -> Result<(RuleLibrary, RuleSource)> {
    RuleLibrary::load_discovered(&rules_candidates())
}

fn collect_targets(lib: &RuleLibrary) -> Vec<ScanTarget> {
    scanner::discover(lib)
}

// ---------------------------------------------------------------- scan

#[tauri::command]
async fn scan_all() -> Result<ScanDto, String> {
    tauri::async_runtime::spawn_blocking(do_scan)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

fn do_scan() -> Result<ScanDto> {
    let (lib, source) = load_library()?;
    let targets = collect_targets(&lib);
    if targets.is_empty() {
        bail!("未发现任何 AI agent 数据目录");
    }
    let outcome = scanner::scan(&targets, &lib);
    let running = procs::detect_running(&lib).unwrap_or_default();

    let agents = outcome
        .agg
        .by_agent
        .iter()
        .map(|(agent, bucket)| BucketDto {
            key: agent.clone(),
            label: lib
                .agents
                .iter()
                .find(|(id, _)| id == agent)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| agent.clone()),
            files: bucket.files,
            bytes: bucket.bytes,
        })
        .collect();

    let classes = outcome
        .agg
        .by_class
        .iter()
        .map(|(class, bucket)| BucketDto {
            key: class_key(*class).to_string(),
            label: class.label().to_string(),
            files: bucket.files,
            bytes: bucket.bytes,
        })
        .collect();

    let roots = outcome
        .roots
        .iter()
        .map(|root| RootDto {
            agent: lib
                .agents
                .iter()
                .find(|(id, _)| id == &root.agent)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| root.agent.clone()),
            path: root.path.clone(),
            files: root.files,
            bytes: root.bytes,
        })
        .collect();

    Ok(ScanDto {
        total_files: outcome.total_files,
        total_bytes: outcome.total_bytes,
        elapsed_ms: outcome.elapsed_ms as u64,
        safe_bytes: outcome.agg.safe_bytes,
        confirm_bytes: outcome.agg.confirm_bytes,
        protected_bytes: outcome.agg.protected_bytes,
        agents,
        classes,
        roots,
        rule_count: lib.len(),
        rule_source: source.label(),
        running,
        generated_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

// ---------------------------------------------------------------- clean

#[tauri::command]
async fn plan_clean(opts: CleanOptions) -> Result<Plan, String> {
    tauri::async_runtime::spawn_blocking(move || do_plan(&opts))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

fn do_plan(opts: &CleanOptions) -> Result<Plan> {
    let (lib, _source) = load_library()?;
    let targets = collect_targets(&lib);
    if targets.is_empty() {
        bail!("未发现任何 AI agent 数据目录");
    }
    let running = procs::detect_running(&lib)?;
    let scan_opts = ScanOptions {
        collect_candidates: true,
        ..Default::default()
    };
    let outcome = scanner::scan_with_opts(&targets, &lib, &scan_opts, |_| {});
    let plan_options = PlanOptions {
        include_review: opts.include_review,
        include_running: opts.include_running,
        min_size: opts.min_size,
        rules: Vec::new(),
        agents: opts.agents.clone(),
    };
    Ok(plan::build(&outcome.candidates, &running, &plan_options))
}

#[tauri::command]
async fn apply_clean(opts: CleanOptions) -> Result<ApplyDto, String> {
    tauri::async_runtime::spawn_blocking(move || do_apply(&opts))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

fn do_apply(opts: &CleanOptions) -> Result<ApplyDto> {
    let plan = do_plan(opts)?;
    if plan.is_empty() {
        bail!("计划为空，没有可执行的内容");
    }
    let quarantine = Quarantine::new(bin_dir());
    let report = quarantine.apply(&plan)?;
    Ok(ApplyDto {
        run_id: report.run_id,
        moved: report.moved as u64,
        bytes: report.bytes,
        failures: report
            .failures
            .into_iter()
            .map(|(path, reason)| (path.display().to_string(), reason))
            .collect(),
        run_dir: report.run_dir.display().to_string(),
    })
}

// ---------------------------------------------------------------- vault

#[tauri::command]
async fn list_runs() -> Result<Vec<RunDto>, String> {
    tauri::async_runtime::spawn_blocking(|| -> Result<Vec<RunDto>> {
        let quarantine = Quarantine::new(bin_dir());
        let runs = quarantine.list_runs()?;
        Ok(runs
            .into_iter()
            .map(|run| RunDto {
                run_id: run.run_id,
                created_at: run.created_at,
                status: status_key(run.status).to_string(),
                items: run.items as u64,
                bytes: run.bytes,
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn restore_run(run_id: String) -> Result<RestoreDto, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<RestoreDto> {
        let quarantine = Quarantine::new(bin_dir());
        let report = quarantine.restore(&run_id)?;
        Ok(RestoreDto {
            restored: report.restored as u64,
            bytes: report.restored_bytes,
            conflicts: report
                .conflicts
                .into_iter()
                .map(|(path, reason)| (path.display().to_string(), reason))
                .collect(),
            failures: report
                .failures
                .into_iter()
                .map(|(path, reason)| (path.display().to_string(), reason))
                .collect(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn bin_usage() -> Result<UsageDto, String> {
    tauri::async_runtime::spawn_blocking(|| -> Result<UsageDto> {
        let dir = bin_dir();
        let quarantine = Quarantine::new(dir.clone());
        let (runs, bytes) = quarantine.usage();
        Ok(UsageDto {
            runs,
            bytes,
            path: dir.display().to_string(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- backup

#[tauri::command]
async fn create_backup(
    label: Option<String>,
    agents: Vec<String>,
    out_dir: Option<String>,
) -> Result<BackupDto, String> {
    tauri::async_runtime::spawn_blocking(move || do_backup(label, agents, out_dir))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

fn do_backup(
    label: Option<String>,
    agents: Vec<String>,
    out_dir: Option<String>,
) -> Result<BackupDto> {
    let (lib, _source) = load_library()?;
    let targets = collect_targets(&lib);
    if targets.is_empty() {
        bail!("未发现任何 AI agent 数据目录");
    }
    let scan_opts = ScanOptions {
        collect_assets: true,
        ..Default::default()
    };
    let outcome = scanner::scan_with_opts(&targets, &lib, &scan_opts, |_| {});

    let nodes: Vec<_> = outcome
        .candidates
        .into_iter()
        .filter(|n| agents.is_empty() || agents.iter().any(|a| a == &n.agent))
        .collect();

    let roots: Vec<String> = outcome.roots.iter().map(|r| r.path.clone()).collect();
    let target = out_dir.map(PathBuf::from).unwrap_or_else(backup_dir);
    let report = backup::create(&nodes, &target, label.as_deref(), &roots)?;
    Ok(BackupDto {
        path: report.path.display().to_string(),
        entries: report.entries as u64,
        source_bytes: report.source_bytes,
        zip_bytes: report.zip_bytes,
        sha256: report.zip_sha256,
        failures: report
            .failures
            .into_iter()
            .map(|(path, reason)| (path.display().to_string(), reason))
            .collect(),
    })
}

#[tauri::command]
async fn list_backups(dir: Option<String>) -> Result<Vec<BackupInfoDto>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<BackupInfoDto>> {
        let dir = dir.map(PathBuf::from).unwrap_or_else(backup_dir);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut items = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("zip") {
                continue;
            }
            let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            // manifest 读不出来说明包损坏，但仍然列出来让用户能看到
            let manifest: Option<BackupManifest> = backup::read_manifest(&path).ok();
            items.push(BackupInfoDto {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                path: path.display().to_string(),
                size_bytes,
                created_at: manifest.as_ref().map(|m| m.created_at.clone()),
                entries: manifest.as_ref().map(|m| m.entries.len()),
                valid: manifest.is_some(),
            });
        }
        items.sort_by(|a, b| b.name.cmp(&a.name));
        Ok(items)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn verify_backup(path: String) -> Result<VerifyDto, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<VerifyDto> {
        let report = backup::verify(std::path::Path::new(&path))?;
        Ok(VerifyDto {
            entries: report.entries,
            ok: report.ok,
            mismatched: report.mismatched,
            missing: report.missing,
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- rules

#[tauri::command]
async fn rules_info() -> Result<RulesInfoDto, String> {
    tauri::async_runtime::spawn_blocking(|| -> Result<RulesInfoDto> {
        let (lib, _source) = load_library().context("加载规则库失败")?;
        Ok(RulesInfoDto {
            count: lib.len(),
            agents: lib.agents.clone(),
            roots: lib.roots.len(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- agents

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentListItem {
    id: String,
    name: String,
    /// 本机是否真的存在数据目录
    installed: bool,
    paths: Vec<String>,
}

/// 给界面做「选择要清理哪个 agent」的候选列表。
/// 一次轻量调用，不扫描文件，只读规则与目录是否存在。
#[tauri::command]
async fn list_agents() -> Result<Vec<AgentListItem>, String> {
    tauri::async_runtime::spawn_blocking(|| -> Result<Vec<AgentListItem>> {
        let (lib, _source) = load_library()?;
        let mut items = Vec::new();
        for (id, name) in &lib.agents {
            let paths: Vec<String> = lib
                .roots
                .iter()
                .filter(|root| &root.agent == id && root.path.exists())
                .map(|root| root.path.display().to_string())
                .collect();
            items.push(AgentListItem {
                id: id.clone(),
                name: name.clone(),
                installed: !paths.is_empty(),
                paths,
            });
        }
        Ok(items)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- main

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            scan_all,
            plan_clean,
            apply_clean,
            list_runs,
            restore_run,
            bin_usage,
            create_backup,
            list_backups,
            verify_backup,
            rules_info,
            list_agents
        ])
        .run(tauri::generate_context!())
        .expect("AgentCurator 启动失败");
}
