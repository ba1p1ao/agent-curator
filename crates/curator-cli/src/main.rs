//! AgentCurator 命令行入口。
//!
//! 安全模型：`scan` 只读；`clean` 默认 dry-run，必须显式 `--apply` 才会移动文件，
//! 且移动一律进隔离区，可用 `restore` 按原路径还原。

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Local;
use clap::{Args, Parser, Subcommand};
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use curator_core::backup;
use curator_core::model::DataClass;
use curator_core::plan::{self, PlanOptions};
use curator_core::procs;
use curator_core::quarantine::{Quarantine, DEFAULT_BIN_DIR};
use curator_core::report::{self, ReportInput};
use curator_core::rules::{RuleLibrary, RuleSource};
use curator_core::scanner::{self, ScanEvent, ScanOptions, ScanTarget};
use curator_core::util::{expand_tilde, home_dir, human_bytes, pct, thousands, truncate};

const ENV_BIN_DIR: &str = "CURATOR_BIN_DIR";
const ENV_RULES_DIR: &str = "CURATOR_RULES_DIR";
const ENV_BACKUP_DIR: &str = "CURATOR_BACKUP_DIR";

#[derive(Parser)]
#[command(
    name = "curator",
    version,
    about = "AgentCurator — AI agent 文件体检、清理与备份",
    long_about = "给 AI agent 留下的文件做清产核资：分清能删的和不能删的，\n\
清理一律先进隔离区，备份逐条校验 sha256。\n\n\
不带子命令时直接体检并给出建议。",
    after_help = "常用：\n\
  curator                 体检 + 建议（只读）\n\
  curator clean           清理计划（默认不动文件）\n\
  curator clean --apply   执行，移入隔离区\n\
  curator restore --last  撤销上一次清理\n\
  curator backup          备份资产\n\
  curator doctor          环境自检"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 环境自检：规则来源、隔离区、备份、进程状态
    Doctor,
    /// 扫描已安装的 AI agent 数据目录并输出体检报告（只读）
    Scan(ScanArgs),
    /// 生成清理计划并移入隔离区（默认 dry-run）
    Clean(CleanArgs),
    /// 从隔离区还原
    Restore(RestoreArgs),
    /// 查看隔离区占用、清理旧批次
    Bin(BinArgs),
    /// 规则库校验与查看
    Rules(RulesArgs),
    /// 备份用户资产（skills / memory / 配置）
    Backup(BackupArgs),
    /// 校验备份包完整性
    BackupVerify(BackupVerifyArgs),
    /// 从备份包还原
    BackupRestore(BackupRestoreArgs),
}

#[derive(Args)]
struct ScanArgs {
    /// 规则目录，默认自动查找 ./rules 或可执行文件旁的 rules
    #[arg(long, value_name = "DIR")]
    rules: Option<PathBuf>,
    /// 手动指定扫描根目录（可重复），覆盖自动发现
    #[arg(long = "root", value_name = "PATH")]
    roots: Vec<String>,
    /// 导出 JSON 报告到指定文件
    #[arg(long, value_name = "FILE")]
    json: Option<PathBuf>,
    /// 导出 Markdown 报告到指定文件
    #[arg(long, value_name = "FILE")]
    md: Option<PathBuf>,
    /// 导出未识别文件清单（TSV：字节数<TAB>路径），用于迭代规则库
    #[arg(long, value_name = "FILE")]
    dump_unmatched: Option<PathBuf>,
}

#[derive(Args)]
struct BackupArgs {
    #[arg(long, value_name = "DIR")]
    rules: Option<PathBuf>,
    #[arg(long = "root", value_name = "PATH")]
    roots: Vec<String>,
    /// 只备份指定 agent（可重复）
    #[arg(long = "agent", value_name = "ID")]
    agent_filter: Vec<String>,
    /// 只备份指定规则（可重复）
    #[arg(long = "rule", value_name = "ID")]
    rule_filter: Vec<String>,
    /// 输出目录，默认 ~/curator-backups
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
    /// 包名标签，例如 before-upgrade
    #[arg(long, value_name = "LABEL")]
    label: Option<String>,
    /// 把 .git / node_modules / 日志这类可重建内容也打进包
    #[arg(long)]
    include_noise: bool,
}

#[derive(Args)]
struct BackupVerifyArgs {
    /// 备份包路径
    zip: PathBuf,
}

#[derive(Args)]
struct BackupRestoreArgs {
    /// 备份包路径
    zip: PathBuf,
    /// 还原到指定目录（保留原始路径结构），默认回到原始绝对路径
    #[arg(long, value_name = "DIR")]
    to: Option<PathBuf>,
    /// 覆盖已存在的文件
    #[arg(long)]
    overwrite: bool,
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct CleanArgs {
    #[arg(long, value_name = "DIR")]
    rules: Option<PathBuf>,
    #[arg(long = "root", value_name = "PATH")]
    roots: Vec<String>,
    /// 连带处理"需人工确认"项（高风险垃圾 / 用户数据 / 项目产物）
    #[arg(long)]
    include_review: bool,
    /// 允许处理正在运行的 agent 目录（风险自负）
    #[arg(long)]
    include_running: bool,
    /// 只处理不小于该体积的单项（字节）
    #[arg(long, value_name = "BYTES", default_value_t = 0)]
    min_size: u64,
    /// 只处理指定规则 id（可重复）
    #[arg(long = "rule", value_name = "ID")]
    rule_filter: Vec<String>,
    /// 只处理指定 agent（可重复）
    #[arg(long = "agent", value_name = "ID")]
    agent_filter: Vec<String>,
    /// 隔离区目录，默认 ~/.curator-bin
    #[arg(long, value_name = "DIR")]
    bin: Option<PathBuf>,
    /// 真正执行（不加此参数只出计划）
    #[arg(long)]
    apply: bool,
    /// 跳过交互确认
    #[arg(long)]
    yes: bool,
    /// 把计划导出为 JSON
    #[arg(long, value_name = "FILE")]
    plan_out: Option<PathBuf>,
}

#[derive(Args)]
struct RestoreArgs {
    /// 批次号，可从 `curator bin` 查看
    run_id: Option<String>,
    /// 还原最近一个批次
    #[arg(long)]
    last: bool,
    #[arg(long, value_name = "DIR")]
    bin: Option<PathBuf>,
    /// 只列出批次，不还原
    #[arg(long)]
    list: bool,
    /// 跳过交互确认
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct BinArgs {
    #[arg(long, value_name = "DIR")]
    bin: Option<PathBuf>,
    /// 物理删除旧批次（保留最新 --keep 个）
    #[arg(long)]
    purge: bool,
    #[arg(long, value_name = "N", default_value_t = 1)]
    keep: usize,
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct RulesArgs {
    #[command(subcommand)]
    cmd: RulesCmd,
}

#[derive(Subcommand)]
enum RulesCmd {
    /// 校验规则文件并打印摘要
    Validate(RulesLoc),
    /// 列出全部规则
    List(RulesLoc),
}

#[derive(Args)]
struct RulesLoc {
    #[arg(long, value_name = "DIR")]
    rules: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let Some(cmd) = cli.cmd else {
        return run_welcome();
    };
    match cmd {
        Cmd::Doctor => run_doctor(),
        Cmd::Scan(args) => run_scan(args),
        Cmd::Clean(args) => run_clean(args),
        Cmd::Restore(args) => run_restore(args),
        Cmd::Bin(args) => run_bin(args),
        Cmd::Rules(args) => match args.cmd {
            RulesCmd::Validate(loc) => run_validate(loc),
            RulesCmd::List(loc) => run_list(loc),
        },
        Cmd::Backup(args) => run_backup(args),
        Cmd::BackupVerify(args) => run_backup_verify(args),
        Cmd::BackupRestore(args) => run_backup_restore(args),
    }
}

// ---------------------------------------------------------------- welcome

/// 不带子命令时的默认行为：体检 + 下一步建议。
///
/// 这是产品的"第一屏"。用户 `curator` 一敲就能知道：
/// 有多少可释放的空间、有没有风险、接下来该敲什么。
fn run_welcome() -> Result<()> {
    let (lib, source) = load_rules(None)?;

    println!("AgentCurator {}  ·  只读体检", env!("CARGO_PKG_VERSION"));
    if source.is_builtin() {
        println!("规则来源：内置（当前目录没有 rules/）");
    }
    println!();

    let targets = build_targets(&[], &lib);
    if targets.is_empty() {
        println!("没有检测到任何 AI agent 数据目录。");
        println!();
        println!("如果你确实装了 Claude Code / Codex / WorkBuddy，");
        println!("请确认它们至少完整运行过一次，或者手动指定目录：");
        println!("  curator scan --root \"C:/Users/you/.claude\"");
        return Ok(());
    }

    let outcome = scanner::scan_with(&targets, &lib, print_scan_event);
    let agg = &outcome.agg;

    println!(
        "\n{} 个文件 / {}，耗时 {} ms\n",
        thousands(outcome.total_files),
        human_bytes(outcome.total_bytes),
        outcome.elapsed_ms
    );

    let mut t = new_table(vec!["类别", "体积", "占比", "结论"]);
    for (class, bucket) in &agg.by_class {
        let verdict = match class {
            DataClass::Asset => "保留，删了不可恢复",
            DataClass::Workspace => "需逐项确认",
            DataClass::Product => "可由源码重建",
            DataClass::Ephemeral => "可安全释放",
            DataClass::Unknown => "保留，无规则命中",
        };
        t.add_row(vec![
            class.label().to_string(),
            human_bytes(bucket.bytes),
            pct(bucket.bytes, agg.total_bytes),
            verdict.to_string(),
        ]);
    }
    println!("{t}");

    println!();
    if agg.safe_bytes > 0 {
        println!(
            "可安全释放 {}（低风险、可重建）。",
            human_bytes(agg.safe_bytes)
        );
    }
    if let Some(bucket) = agg.by_class.get(&DataClass::Unknown) {
        if bucket.bytes > 0 {
            println!(
                "另有 {} 个文件没有规则命中（{}），默认保留。",
                thousands(bucket.files),
                human_bytes(bucket.bytes)
            );
        }
    }

    println!("\n下一步");
    println!("  curator clean      查看清理计划（不会动任何文件）");
    println!("  curator backup     备份用户资产（skills / memory / 配置）");
    println!("  curator doctor     查看环境、规则与隔离区状态");
    Ok(())
}

// ---------------------------------------------------------------- doctor

/// 环境自检。出问题时第一个该跑的命令。
fn run_doctor() -> Result<()> {
    let (lib, source) = load_rules(None)?;

    println!("环境");
    println!("  版本     {}", env!("CARGO_PKG_VERSION"));
    println!("  规则     {} 条 / {} 个文件", lib.len(), lib.sources.len());
    println!("  来源     {}", source.label());
    if source.is_builtin() {
        println!("           想热改规则，就在当前目录建一个 rules/ 目录");
    }

    let targets = scanner::discover(&lib);
    println!(
        "  扫描根   {} 个定义，本机命中 {} 个",
        lib.roots.len(),
        targets.len()
    );

    let bin = resolve_bin_dir(None);
    let quarantine = Quarantine::new(bin.clone());
    let (runs, bytes) = quarantine.usage();
    let writable = fs::create_dir_all(&bin).is_ok();
    println!(
        "  隔离区   {} （{} 批 / {}）{}",
        bin.display(),
        runs,
        human_bytes(bytes),
        if writable { "" } else { "  [不可写]" }
    );

    let backups = resolve_backup_dir(None);
    let packages = fs::read_dir(&backups)
        .map(|it| {
            it.flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("zip"))
                .count()
        })
        .unwrap_or(0);
    println!("  备份     {} （{} 个包）", backups.display(), packages);

    println!("\nAgent");
    for root in &lib.roots {
        let installed = root.path.exists();
        println!(
            "  {:<14} {:<8} {}",
            root.agent,
            if installed { "已安装" } else { "未安装" },
            if installed {
                root.path.display().to_string()
            } else {
                String::new()
            }
        );
    }

    println!("\n进程");
    match procs::detect_running(&lib) {
        Ok(list) if list.is_empty() => println!("  没有运行中的 agent，可以安全清理"),
        Ok(list) => println!("  正在运行：{}（清理时这些会被跳过）", list.join("、")),
        Err(e) => println!("  检测失败：{}（清理会中止，除非加 --include-running）", e),
    }

    println!("\n提示");
    println!("  curator                 直接体检并给建议");
    println!("  curator clean           查看清理计划");
    println!("  curator rules validate  校验规则文件");
    Ok(())
}

// ---------------------------------------------------------------- scan

fn run_scan(args: ScanArgs) -> Result<()> {
    let (lib, source) = load_rules(args.rules.clone())?;

    eprintln!(
        "规则库: {} 条规则 / {} 个文件  （{}）",
        lib.len(),
        lib.sources.len(),
        source.label()
    );
    for warning in &lib.warnings {
        eprintln!("[警告] {}", warning);
    }

    let targets = build_targets(&args.roots, &lib);
    if targets.is_empty() {
        bail!(
            "未发现任何 AI agent 数据目录。\n\
             已检测的根目录均不存在或缺少特征文件。\n\
             可用 --root <PATH> 手动指定一个目录后重试。"
        );
    }

    eprintln!("开始只读扫描，共 {} 个根目录\n", targets.len());

    let outcome = scanner::scan_with(&targets, &lib, print_scan_event);

    eprintln!();

    let input = ReportInput {
        agg: &outcome.agg,
        roots: &outcome.roots,
        elapsed_ms: outcome.elapsed_ms,
        rule_count: lib.len(),
        sources: &lib.sources,
        agent_names: &lib.agents,
        generated_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };

    print!("{}", report::render_terminal(&input));

    if let Some(path) = &args.json {
        write_file(path, &report::render_json(&input))?;
    }
    if let Some(path) = &args.md {
        write_file(path, &report::render_markdown(&input))?;
    }
    if let Some(path) = &args.dump_unmatched {
        let mut text = String::new();
        for item in &outcome.agg.unmatched {
            text.push_str(&format!(
                "{}\t{}\n",
                item.bytes,
                item.path.to_string_lossy().replace('\\', "/")
            ));
        }
        write_file(path, &text)?;
        if outcome.agg.unmatched_truncated {
            eprintln!("[提示] 未识别清单已截断，仅保留前 2000 条");
        }
    }

    println!(
        "
下一步"
    );
    println!("  curator clean     查看清理计划（不会动任何文件）");
    println!("  curator backup    备份用户资产");
    Ok(())
}

// ---------------------------------------------------------------- clean

fn run_clean(args: CleanArgs) -> Result<()> {
    let (lib, source) = load_rules(args.rules.clone())?;
    if source.is_builtin() {
        eprintln!("[提示] 未找到外部 rules/，使用内置规则");
    }

    let targets = build_targets(&args.roots, &lib);
    if targets.is_empty() {
        bail!("未发现任何 AI agent 数据目录，可用 --root <PATH> 指定");
    }

    // 进程检测必须在扫描之前：让用户知道这次结论是在什么前提下得出的
    let running = match procs::detect_running(&lib) {
        Ok(list) => list,
        Err(e) => {
            if !args.include_running {
                bail!(
                    "无法读取进程表，不能确认 agent 是否在运行：{}\n\
                     为避免误删正在写入的数据，已中止。\n\
                     确认相关 agent 已退出后，可加 --include-running 强制执行。",
                    e
                );
            }
            eprintln!("[警告] 进程检测失败（{}），按 --include-running 继续", e);
            Vec::new()
        }
    };
    if !running.is_empty() {
        eprintln!("[注意] 检测到正在运行的 agent: {}", running.join(", "));
        if args.include_running {
            eprintln!("[危险] 已指定 --include-running，相关目录仍会被处理");
        } else {
            eprintln!("[安全] 这些 agent 的条目本次一律跳过，请退出它们后重跑");
        }
    }

    eprintln!("扫描中 ...");
    let scan_opts = ScanOptions {
        collect_candidates: true,
        ..Default::default()
    };
    let outcome = scanner::scan_with_opts(&targets, &lib, &scan_opts, print_scan_event);

    let plan_options = PlanOptions {
        include_review: args.include_review,
        include_running: args.include_running,
        min_size: args.min_size,
        rules: args.rule_filter.clone(),
        agents: args.agent_filter.clone(),
    };
    let plan = plan::build(&outcome.candidates, &running, &plan_options);

    print!("{}", report::render_plan(&plan));

    if let Some(path) = &args.plan_out {
        write_file(path, &serde_json::to_string_pretty(&plan)?)?;
    }

    if !args.apply {
        return Ok(());
    }
    if plan.is_empty() {
        return Ok(());
    }

    if !args.yes {
        let ok = confirm(&format!(
            "\n确认把 {} 项 / {} 移入隔离区？输入 yes 继续: ",
            thousands(plan.items.len() as u64),
            human_bytes(plan.total_bytes())
        ))?;
        if !ok {
            println!("已取消，未做任何改动。");
            return Ok(());
        }
    }

    let bin_dir = resolve_bin_dir(args.bin.clone());
    let quarantine = Quarantine::new(bin_dir.clone());
    eprintln!("隔离区: {}", bin_dir.display());

    let report = quarantine.apply(&plan)?;
    print!("\n{}", report::render_apply(&report));
    Ok(())
}

// ---------------------------------------------------------------- restore

fn run_restore(args: RestoreArgs) -> Result<()> {
    let bin_dir = resolve_bin_dir(args.bin.clone());
    let quarantine = Quarantine::new(bin_dir.clone());

    if args.list {
        print_runs(&quarantine)?;
        return Ok(());
    }

    let run_id = match (&args.run_id, args.last) {
        (Some(id), _) => id.clone(),
        (None, true) => {
            let runs = quarantine.list_runs()?;
            match runs.first() {
                Some(run) => run.run_id.clone(),
                None => bail!("隔离区里没有任何批次"),
            }
        }
        (None, false) => bail!("请指定批次号，或用 --last 还原最近一批，--list 查看全部"),
    };

    let manifest = quarantine.load_manifest(&run_id)?;
    println!(
        "批次 {}  ·  {}  ·  {} 项 / {}",
        manifest.run_id,
        manifest.created_at,
        thousands(manifest.items.len() as u64),
        human_bytes(manifest.items.iter().map(|i| i.bytes).sum())
    );

    if !args.yes {
        let ok = confirm("\n确认还原以上批次？输入 yes 继续: ")?;
        if !ok {
            println!("已取消。");
            return Ok(());
        }
    }

    let report = quarantine.restore(&run_id)?;
    print!("{}", report::render_restore(&report));
    Ok(())
}

// ---------------------------------------------------------------- bin

fn run_bin(args: BinArgs) -> Result<()> {
    let bin_dir = resolve_bin_dir(args.bin.clone());
    let quarantine = Quarantine::new(bin_dir.clone());

    if args.purge {
        let runs = quarantine.list_runs()?;
        if runs.len() <= args.keep {
            println!("只有 {} 个批次，无需清理。", runs.len());
            return Ok(());
        }
        let victims = runs.len() - args.keep;
        let bytes: u64 = runs.iter().skip(args.keep).map(|r| r.bytes).sum();
        println!(
            "将物理删除 {} 个旧批次 / {}（保留最新 {} 个）",
            victims,
            human_bytes(bytes),
            args.keep
        );
        if !args.yes {
            let ok = confirm("\n此操作不可恢复，输入 yes 继续: ")?;
            if !ok {
                println!("已取消。");
                return Ok(());
            }
        }
        let removed = quarantine.purge(args.keep)?;
        for id in &removed {
            println!("  已删除 {}", id);
        }
        return Ok(());
    }

    println!("隔离区: {}", bin_dir.display());
    if !bin_dir.exists() {
        println!("尚未创建（还没有执行过 clean）");
        return Ok(());
    }
    let (count, bytes) = quarantine.usage();
    println!("批次 {} 个 / 占用 {}\n", count, human_bytes(bytes));
    print_runs(&quarantine)?;
    Ok(())
}

fn print_runs(quarantine: &Quarantine) -> Result<()> {
    let runs = quarantine.list_runs()?;
    if runs.is_empty() {
        println!("（无批次）");
        return Ok(());
    }
    let mut table = new_table(vec!["批次号", "时间", "状态", "文件数", "体积"]);
    for run in &runs {
        table.add_row(vec![
            run.run_id.clone(),
            run.created_at.clone(),
            run.status.label().to_string(),
            thousands(run.items as u64),
            human_bytes(run.bytes),
        ]);
    }
    println!("{table}");
    println!("还原：curator restore <批次号>");
    Ok(())
}

// ---------------------------------------------------------------- backup

fn run_backup(args: BackupArgs) -> Result<()> {
    let (lib, _source) = load_rules(args.rules.clone())?;

    let targets = build_targets(&args.roots, &lib);
    if targets.is_empty() {
        bail!("未发现任何 AI agent 数据目录，可用 --root <PATH> 指定");
    }

    eprintln!("扫描资产中 ...");
    let scan_opts = ScanOptions {
        collect_assets: true,
        ..Default::default()
    };
    let outcome = scanner::scan_with_opts(&targets, &lib, &scan_opts, print_scan_event);

    let nodes: Vec<_> = outcome
        .candidates
        .into_iter()
        .filter(|n| {
            (args.agent_filter.is_empty() || args.agent_filter.iter().any(|a| a == &n.agent))
                && (args.rule_filter.is_empty()
                    || n.rule_id
                        .as_ref()
                        .map(|r| args.rule_filter.iter().any(|w| w == r))
                        .unwrap_or(false))
        })
        .collect();

    if nodes.is_empty() {
        bail!("没有匹配到任何资产条目（检查 --agent / --rule 过滤条件）");
    }

    let out_dir = resolve_backup_dir(args.out.clone());
    let root_strings: Vec<String> = outcome.roots.iter().map(|r| r.path.clone()).collect();

    let options = backup::BackupOptions {
        label: args.label.clone(),
        roots: root_strings,
        include_noise: args.include_noise,
    };
    let report = backup::create(&nodes, &out_dir, &options)?;

    println!(
        "备份完成：{} 项 / {} → {}",
        thousands(report.entries as u64),
        human_bytes(report.source_bytes),
        human_bytes(report.zip_bytes)
    );
    if report.source_bytes > 0 {
        let saved = 100.0 - (report.zip_bytes as f64 / report.source_bytes as f64 * 100.0);
        println!("压缩比   {:.1}%", saved);
    }
    println!("备份包   {}", report.path.display());
    println!("校验和   {}", &report.zip_sha256[..16]);
    if report.excluded_files > 0 {
        println!(
            "已排除   {} 个元数据文件 / {}（.git / node_modules / 日志；要保留请加 --include-noise）",
            thousands(report.excluded_files as u64),
            human_bytes(report.excluded_bytes)
        );
    }
    if !report.failures.is_empty() {
        println!("\n{} 项读取失败（未写入备份）：", report.failures.len());
        for (path, err) in report.failures.iter().take(10) {
            println!("  - {} : {}", truncate(&path.to_string_lossy(), 56), err);
        }
    }
    Ok(())
}

fn run_backup_verify(args: BackupVerifyArgs) -> Result<()> {
    let manifest = backup::read_manifest(&args.zip)?;
    println!("备份包   {}", args.zip.display());
    println!(
        "格式     v{} · {} · {}",
        manifest.format_version, manifest.tool, manifest.created_at
    );
    println!("条目     {} 项", thousands(manifest.entries.len() as u64));

    let report = backup::verify(&args.zip)?;
    println!(
        "校验     {} 通过 / {} 不匹配 / {} 缺失",
        report.ok,
        report.mismatched.len(),
        report.missing.len()
    );
    for (name, detail) in report.mismatched.iter().take(10) {
        println!("  不匹配 {} : {}", truncate(name, 50), detail);
    }
    for name in report.missing.iter().take(10) {
        println!("  缺失   {}", truncate(name, 50));
    }
    if !report.mismatched.is_empty() || !report.missing.is_empty() {
        bail!("备份包已损坏或被篡改");
    }
    Ok(())
}

fn run_backup_restore(args: BackupRestoreArgs) -> Result<()> {
    let manifest = backup::read_manifest(&args.zip)?;
    let bytes: u64 = manifest.entries.iter().map(|e| e.bytes).sum();
    println!(
        "备份包   {}  ·  {} 项 / {}",
        args.zip.display(),
        thousands(manifest.entries.len() as u64),
        human_bytes(bytes)
    );
    match &args.to {
        Some(dir) => println!("还原到   {}（按原始路径结构展开）", dir.display()),
        None => println!("还原到   原始绝对路径"),
    }
    if args.overwrite {
        println!("[注意] 已开启 --overwrite，同名文件会被覆盖");
    }

    if !args.yes {
        let ok = confirm("\n确认还原？输入 yes 继续: ")?;
        if !ok {
            println!("已取消。");
            return Ok(());
        }
    }

    let report = backup::restore(&args.zip, args.to.as_deref(), args.overwrite)?;
    println!(
        "\n已还原 {} 项 / {}",
        thousands(report.restored as u64),
        human_bytes(report.bytes)
    );
    if !report.skipped.is_empty() {
        println!("跳过 {} 项（目标已存在，未覆盖）：", report.skipped.len());
        for (path, reason) in report.skipped.iter().take(10) {
            println!("  - {} : {}", truncate(&path.to_string_lossy(), 56), reason);
        }
    }
    if !report.failed.is_empty() {
        println!("失败 {} 项：", report.failed.len());
        for (path, err) in report.failed.iter().take(10) {
            println!("  - {} : {}", truncate(&path.to_string_lossy(), 56), err);
        }
        bail!("部分条目还原失败");
    }
    Ok(())
}

fn resolve_backup_dir(arg: Option<PathBuf>) -> PathBuf {
    if let Some(path) = arg {
        return path;
    }
    if let Some(env) = std::env::var_os(ENV_BACKUP_DIR) {
        return PathBuf::from(env);
    }
    home_dir().join("curator-backups")
}

// ---------------------------------------------------------------- rules

fn run_validate(loc: RulesLoc) -> Result<()> {
    let (lib, source) = load_rules(loc.rules)?;

    println!("规则库校验通过");
    println!("  来源    : {}", source.label());
    println!("  文件    : {} 个", lib.sources.len());
    println!(
        "  Agent   : {}",
        lib.agents
            .iter()
            .map(|(id, name)| format!("{}({})", name, id))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  规则    : {} 条", lib.len());
    println!(
        "  扫描根  : {} 个定义，本机命中 {} 个",
        lib.roots.len(),
        scanner::discover(&lib).len()
    );

    if lib.warnings.is_empty() {
        println!("  警告    : 无");
    } else {
        println!("  警告    : {}", lib.warnings.len());
        for warning in &lib.warnings {
            println!("    - {}", warning);
        }
    }
    Ok(())
}

fn run_list(loc: RulesLoc) -> Result<()> {
    let (lib, _source) = load_rules(loc.rules)?;

    let mut table = new_table(vec!["规则 ID", "类别", "风险", "处置", "匹配模式"]);
    for entry in lib.entries() {
        table.add_row(vec![
            entry.id.clone(),
            entry.class.label().to_string(),
            entry.risk.label().to_string(),
            entry.action.label().to_string(),
            truncate(&entry.pattern, 52),
        ]);
    }
    println!("{table}");
    println!("共 {} 条规则", lib.len());
    Ok(())
}

// ---------------------------------------------------------------- helpers

fn new_table(header: Vec<&str>) -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Disabled);
    table.set_header(header);
    table
}

fn print_scan_event(event: ScanEvent<'_>) {
    match event {
        ScanEvent::RootStarted {
            index,
            total,
            target,
        } => {
            eprint!(
                "  [{}/{}] {}  {} ... ",
                index + 1,
                total,
                target.agent,
                truncate(&target.path.to_string_lossy(), 40)
            );
        }
        ScanEvent::RootFinished { stat, elapsed_ms } => {
            let err = if stat.errors > 0 {
                format!("  [{} 个读取错误]", stat.errors)
            } else {
                String::new()
            };
            eprintln!(
                "{} 个文件 / {}  ({} ms){}",
                thousands(stat.files),
                human_bytes(stat.bytes),
                elapsed_ms,
                err
            );
        }
    }
}

fn build_targets(raw_roots: &[String], lib: &RuleLibrary) -> Vec<ScanTarget> {
    if raw_roots.is_empty() {
        return scanner::discover(lib);
    }
    raw_roots
        .iter()
        .map(|raw| {
            let path = expand_tilde(raw);
            let agent = lib
                .roots
                .iter()
                .find(|root| root.path == path)
                .map(|root| root.agent.clone())
                .unwrap_or_else(|| "custom".to_string());
            ScanTarget {
                agent,
                root_id: "cli".to_string(),
                path,
            }
        })
        .collect()
}

fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
    }
    fs::write(path, content).with_context(|| format!("写入失败: {}", path.display()))?;
    eprintln!("已导出: {}", path.display());
    Ok(())
}

/// 交互确认。读不到输入（非交互环境）一律视为取消。
fn confirm(prompt: &str) -> Result<bool> {
    eprint!("{}", prompt);
    io::stderr().flush().ok();
    let mut line = String::new();
    let read = io::stdin().read_line(&mut line)?;
    if read == 0 {
        eprintln!();
        return Ok(false);
    }
    Ok(line.trim().eq_ignore_ascii_case("yes"))
}

fn resolve_bin_dir(arg: Option<PathBuf>) -> PathBuf {
    if let Some(path) = arg {
        return path;
    }
    if let Some(env) = std::env::var_os(ENV_BIN_DIR) {
        return PathBuf::from(env);
    }
    home_dir().join(DEFAULT_BIN_DIR)
}

/// 规则加载：显式目录 → 环境变量 → 当前目录 → 可执行文件旁 → 内置。
///
/// 最后一级兜底是内置规则，所以用户只拿到一个 exe 也能正常跑。
fn load_rules(arg: Option<PathBuf>) -> Result<(RuleLibrary, RuleSource)> {
    if let Some(path) = arg {
        if !path.is_dir() {
            bail!("规则目录不存在: {}", path.display());
        }
        return Ok((RuleLibrary::load_dir(&path)?, RuleSource::External(path)));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(env) = std::env::var_os(ENV_RULES_DIR) {
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
        }
    }

    RuleLibrary::load_discovered(&candidates)
}
