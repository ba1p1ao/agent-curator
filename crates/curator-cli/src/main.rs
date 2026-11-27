//! AgentCurator 命令行入口（M1 只读体检）。
//!
//! 设计约束：本二进制不包含任何删除 / 移动代码路径。
//! 清理与备份能力将在 M2 引入，且必须走「计划 → 人工确认 → 隔离区」流程。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Local;
use clap::{Args, Parser, Subcommand};
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use curator_core::report::{self, ReportInput};
use curator_core::rules::RuleLibrary;
use curator_core::scanner::{self, ScanEvent, ScanTarget};
use curator_core::util::{expand_tilde, human_bytes, thousands, truncate};

#[derive(Parser)]
#[command(
    name = "curator",
    version,
    about = "AgentCurator — AI agent 文件体检（M1 只读）"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 扫描已安装的 AI agent 数据目录并输出体检报告（只读，不改动任何文件）
    Scan(ScanArgs),
    /// 规则库校验与查看
    Rules(RulesArgs),
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
    match cli.cmd {
        Cmd::Scan(args) => run_scan(args),
        Cmd::Rules(args) => match args.cmd {
            RulesCmd::Validate(loc) => run_validate(loc),
            RulesCmd::List(loc) => run_list(loc),
        },
    }
}

fn run_scan(args: ScanArgs) -> Result<()> {
    let rules_dir = resolve_rules_dir(args.rules.clone())?;
    let lib = RuleLibrary::load_dir(&rules_dir)
        .with_context(|| format!("加载规则库失败: {}", rules_dir.display()))?;

    eprintln!(
        "规则库: {} 条规则 / {} 个文件  ({})",
        lib.len(),
        lib.sources.len(),
        rules_dir.display()
    );
    for warning in &lib.warnings {
        eprintln!("[警告] {}", warning);
    }

    let targets = build_targets(&args, &lib);
    if targets.is_empty() {
        bail!(
            "未发现任何 AI agent 数据目录。\n\
             已检测的根目录均不存在或缺少特征文件。\n\
             可用 --root <PATH> 手动指定一个目录后重试。"
        );
    }

    eprintln!("开始只读扫描，共 {} 个根目录\n", targets.len());

    let outcome = scanner::scan_with(&targets, &lib, |event| match event {
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
    });

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
        write_report(path, &report::render_json(&input))?;
    }
    if let Some(path) = &args.md {
        write_report(path, &report::render_markdown(&input))?;
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
        write_report(path, &text)?;
        if outcome.agg.unmatched_truncated {
            eprintln!("[提示] 未识别清单已截断，仅保留前 2000 条");
        }
    }

    Ok(())
}

fn write_report(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
    }
    fs::write(path, content).with_context(|| format!("写入报告失败: {}", path.display()))?;
    eprintln!("已导出: {}", path.display());
    Ok(())
}

fn build_targets(args: &ScanArgs, lib: &RuleLibrary) -> Vec<ScanTarget> {
    if args.roots.is_empty() {
        return scanner::discover(lib);
    }
    args.roots
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

fn run_validate(loc: RulesLoc) -> Result<()> {
    let dir = resolve_rules_dir(loc.rules)?;
    let lib = RuleLibrary::load_dir(&dir)?;

    println!("规则库校验通过");
    println!("  目录    : {}", dir.display());
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
    let dir = resolve_rules_dir(loc.rules)?;
    let lib = RuleLibrary::load_dir(&dir)?;

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Disabled);
    table.set_header(vec!["规则 ID", "类别", "风险", "处置", "匹配模式"]);

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

/// 依次尝试：命令行参数 → 环境变量 → 当前目录 → 可执行文件向上若干层。
fn resolve_rules_dir(arg: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = arg {
        return Ok(path);
    }
    if let Some(env) = std::env::var_os("CURATOR_RULES_DIR") {
        return Ok(PathBuf::from(env));
    }

    let cwd = std::env::current_dir().context("无法获取当前工作目录")?;
    let local = cwd.join("rules");
    if local.is_dir() {
        return Ok(local);
    }

    if let Ok(exe) = std::env::current_exe() {
        let mut cursor = exe.parent().map(|p| p.to_path_buf());
        let mut depth = 0;
        while let Some(dir) = cursor {
            let candidate = dir.join("rules");
            if candidate.is_dir() {
                return Ok(candidate);
            }
            if depth >= 4 {
                break;
            }
            cursor = dir.parent().map(|p| p.to_path_buf());
            depth += 1;
        }
    }

    bail!("找不到规则目录，请用 --rules <DIR> 指定，或设置环境变量 CURATOR_RULES_DIR")
}
