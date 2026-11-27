//! 报表渲染：终端表格 / Markdown / JSON。
//!
//! 报表只陈述事实与建议，"可释放" 全部来自规则判定，
//! 不在这里做任何二次推断。

use std::path::PathBuf;

use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use serde_json::json;

use crate::aggregate::Aggregates;
use crate::model::DataClass;
use crate::plan::Plan;
use crate::quarantine::{ApplyReport, RestoreReport};
use crate::scanner::RootStat;
use crate::util::{human_bytes, pct, thousands, truncate};

pub struct ReportInput<'a> {
    pub agg: &'a Aggregates,
    pub roots: &'a [RootStat],
    pub elapsed_ms: u128,
    pub rule_count: usize,
    pub sources: &'a [PathBuf],
    pub agent_names: &'a [(String, String)],
    pub generated_at: String,
}

fn new_table(header: Vec<&str>) -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Disabled);
    table.set_header(header);
    table
}

fn display_name(input: &ReportInput<'_>, agent: &str) -> String {
    input
        .agent_names
        .iter()
        .find(|(id, _)| id == agent)
        .map(|(_, name)| name.clone())
        .unwrap_or_else(|| agent.to_string())
}

/// 终端人读报表。所有数字右对齐会让宽度抖动，因此统一左对齐。
pub fn render_terminal(input: &ReportInput) -> String {
    let agg = input.agg;
    let mut out = String::new();

    out.push_str(&format!(
        "AgentCurator v{}  ·  只读体检模式（不会删除、移动任何文件）\n",
        env!("CARGO_PKG_VERSION")
    ));
    out.push_str(&format!(
        "生成时间 {}  ·  耗时 {} ms  ·  规则 {} 条（来自 {} 个文件）\n\n",
        input.generated_at,
        input.elapsed_ms,
        input.rule_count,
        input.sources.len()
    ));

    // 1. 扫描根
    let mut t = new_table(vec!["Agent", "物理路径", "文件数", "体积", "错误"]);
    for root in input.roots {
        t.add_row(vec![
            display_name(input, &root.agent),
            truncate(&root.path, 46),
            thousands(root.files),
            human_bytes(root.bytes),
            roots_errors(root.errors),
        ]);
    }
    out.push_str("扫描范围\n");
    out.push_str(&t.to_string());
    out.push_str("\n\n");

    // 2. Agent 分布
    let mut t = new_table(vec!["Agent", "文件数", "体积", "占比"]);
    for (agent, bucket) in &agg.by_agent {
        t.add_row(vec![
            display_name(input, agent),
            thousands(bucket.files),
            human_bytes(bucket.bytes),
            pct(bucket.bytes, agg.total_bytes),
        ]);
    }
    t.add_row(vec![
        "合计".to_string(),
        thousands(agg.total_files),
        human_bytes(agg.total_bytes),
        "100.0%".to_string(),
    ]);
    out.push_str("按 Agent 分布\n");
    out.push_str(&t.to_string());
    out.push_str("\n\n");

    // 3. 数据性质
    let mut t = new_table(vec!["类别", "文件数", "体积", "占比", "判定说明"]);
    for (class, bucket) in &agg.by_class {
        t.add_row(vec![
            class.label().to_string(),
            thousands(bucket.files),
            human_bytes(bucket.bytes),
            pct(bucket.bytes, agg.total_bytes),
            class.hint().to_string(),
        ]);
    }
    out.push_str("按数据性质分类（核心结论）\n");
    out.push_str(&t.to_string());
    out.push_str("\n\n");

    // 4. 释放潜力
    out.push_str("释放潜力\n");
    out.push_str(&format!(
        "  可安全释放（低风险可重建）      {}{}\n",
        human_bytes(agg.safe_bytes),
        ratio_note(agg.safe_bytes, agg.total_bytes)
    ));
    out.push_str(&format!(
        "  需人工确认（高风险 / 项目产物）  {}{}\n",
        human_bytes(agg.confirm_bytes),
        ratio_note(agg.confirm_bytes, agg.total_bytes)
    ));
    out.push_str(&format!(
        "  受保护（资产 + 未识别）          {}{}\n",
        human_bytes(agg.protected_bytes),
        ratio_note(agg.protected_bytes, agg.total_bytes)
    ));

    // 未识别比例偏高说明规则库有盲区，直接给出下一步动作
    if let Some((unmatched_files, unmatched_bytes)) = agg
        .by_class
        .get(&DataClass::Unknown)
        .map(|b| (b.files, b.bytes))
    {
        if unmatched_bytes > 0 {
            out.push_str(&format!(
                "  未识别 {} 个文件 / {}，可用 --dump-unmatched <FILE> 导出清单补充规则\n",
                thousands(unmatched_files),
                human_bytes(unmatched_bytes)
            ));
        }
    }
    out.push('\n');

    // 5. 规则 Top 12
    let rules = agg.rules_by_size();
    let mut t = new_table(vec!["规则 ID", "类别", "风险", "处置", "文件数", "体积"]);
    for rule in rules.iter().take(12) {
        t.add_row(vec![
            truncate(&rule.rule_id, 34),
            rule.class.label().to_string(),
            rule.risk.label().to_string(),
            rule.action.label().to_string(),
            thousands(rule.files),
            human_bytes(rule.bytes),
        ]);
    }
    out.push_str("按规则体积 Top 12\n");
    out.push_str(&t.to_string());
    if rules.len() > 12 {
        out.push_str(&format!(
            "\n（共 {} 条规则命中，其余省略）",
            rules.len()
        ));
    }
    out.push_str("\n\n");

    // 6. 最大单项
    if !agg.largest.is_empty() {
        let mut t = new_table(vec!["最大可清理项", "类别", "体积", "命中规则"]);
        for item in agg.largest.iter().take(10) {
            t.add_row(vec![
                truncate(&item.path.to_string_lossy().replace('\\', "/"), 56),
                item.class.label().to_string(),
                human_bytes(item.bytes),
                item.rule_id.clone().unwrap_or_else(|| "-".to_string()),
            ]);
        }
        out.push_str("最大单项 Top 10\n");
        out.push_str(&t.to_string());
        out.push_str("\n\n");
    }

    out.push_str("说明：本报告为只读体检结果，未对任何文件做修改。\n");
    out.push_str("M1 阶段不提供删除能力；清理与备份将在 M2 实现，且必须经人工确认后走隔离区。\n");

    out
}

fn roots_errors(errors: u64) -> String {
    if errors == 0 {
        "-".to_string()
    } else {
        errors.to_string()
    }
}

fn ratio_note(part: u64, total: u64) -> String {
    if total == 0 || part == 0 {
        return String::new();
    }
    format!("  ({})", pct(part, total))
}

/// Markdown 报表，便于贴进 Issue / 周报。
pub fn render_markdown(input: &ReportInput) -> String {
    let agg = input.agg;
    let mut out = String::new();

    out.push_str("# AgentCurator 体检报告\n\n");
    out.push_str(&format!("- 生成时间：{}\n", input.generated_at));
    out.push_str(&format!("- 扫描耗时：{} ms\n", input.elapsed_ms));
    out.push_str(&format!(
        "- 规则：{} 条 / {} 个文件\n\n",
        input.rule_count,
        input.sources.len()
    ));
    out.push_str("> 只读体检，未修改任何文件。\n\n");

    out.push_str("## 扫描范围\n\n");
    out.push_str("| Agent | 路径 | 文件数 | 体积 |\n|---|---|---|---|\n");
    for root in input.roots {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            display_name(input, &root.agent),
            root.path,
            thousands(root.files),
            human_bytes(root.bytes)
        ));
    }

    out.push_str("\n## 按 Agent 分布\n\n");
    out.push_str("| Agent | 文件数 | 体积 | 占比 |\n|---|---|---|---|\n");
    for (agent, bucket) in &agg.by_agent {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            display_name(input, agent),
            thousands(bucket.files),
            human_bytes(bucket.bytes),
            pct(bucket.bytes, agg.total_bytes)
        ));
    }

    out.push_str("\n## 按数据性质分类\n\n");
    out.push_str("| 类别 | 文件数 | 体积 | 占比 | 说明 |\n|---|---|---|---|---|\n");
    for (class, bucket) in &agg.by_class {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            class.label(),
            thousands(bucket.files),
            human_bytes(bucket.bytes),
            pct(bucket.bytes, agg.total_bytes),
            class.hint()
        ));
    }

    out.push_str("\n## 释放潜力\n\n");
    out.push_str(&format!(
        "- 可安全释放：**{}**\n- 需人工确认：**{}**\n- 受保护：**{}**\n",
        human_bytes(agg.safe_bytes),
        human_bytes(agg.confirm_bytes),
        human_bytes(agg.protected_bytes)
    ));

    out.push_str("\n## 按规则体积 Top 12\n\n");
    out.push_str("| 规则 ID | 类别 | 风险 | 处置 | 文件数 | 体积 |\n|---|---|---|---|---|---|\n");
    for rule in agg.rules_by_size().iter().take(12) {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} |\n",
            rule.rule_id,
            rule.class.label(),
            rule.risk.label(),
            rule.action.label(),
            thousands(rule.files),
            human_bytes(rule.bytes)
        ));
    }

    if !agg.largest.is_empty() {
        out.push_str("\n## 最大单项 Top 10\n\n");
        out.push_str("| 路径 | 类别 | 体积 |\n|---|---|---|\n");
        for item in agg.largest.iter().take(10) {
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                item.path.to_string_lossy().replace('\\', "/"),
                item.class.label(),
                human_bytes(item.bytes)
            ));
        }
    }

    out
}

/// 机器可读 JSON，给后续 Tauri 前端 / 自动化用。
pub fn render_json(input: &ReportInput) -> String {
    let agg = input.agg;

    let roots: Vec<_> = input
        .roots
        .iter()
        .map(|r| {
            json!({
                "agent": r.agent.clone(),
                "root_id": r.root_id.clone(),
                "path": r.path.clone(),
                "files": r.files,
                "bytes": r.bytes,
                "errors": r.errors,
            })
        })
        .collect();

    let by_agent: Vec<_> = agg
        .by_agent
        .iter()
        .map(|(agent, b)| {
            json!({
                "agent": agent.clone(),
                "files": b.files,
                "bytes": b.bytes,
                "ratio": ratio(b.bytes, agg.total_bytes),
            })
        })
        .collect();

    let by_class: Vec<_> = agg
        .by_class
        .iter()
        .map(|(class, b)| {
            json!({
                "class": class,
                "label": class.label(),
                "files": b.files,
                "bytes": b.bytes,
                "ratio": ratio(b.bytes, agg.total_bytes),
            })
        })
        .collect();

    let by_rule: Vec<_> = agg
        .rules_by_size()
        .iter()
        .map(|r| {
            json!({
                "rule_id": r.rule_id.clone(),
                "agent": r.agent.clone(),
                "class": r.class,
                "risk": r.risk,
                "action": r.action,
                "files": r.files,
                "bytes": r.bytes,
                "sample": r.sample.as_ref().map(|p| p.to_string_lossy().replace('\\', "/")),
                "note": r.note.clone(),
            })
        })
        .collect();

    let largest: Vec<_> = agg
        .largest
        .iter()
        .map(|i| {
            json!({
                "path": i.path.to_string_lossy().replace('\\', "/"),
                "class": i.class,
                "bytes": i.bytes,
                "rule_id": i.rule_id.clone(),
            })
        })
        .collect();

    let sources: Vec<String> = input
        .sources
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();

    let value = json!({
        "tool": "AgentCurator",
        "version": env!("CARGO_PKG_VERSION"),
        "mode": "read-only",
        "generated_at": input.generated_at.clone(),
        "elapsed_ms": input.elapsed_ms as u64,
        "rules": { "count": input.rule_count, "sources": sources },
        "totals": {
            "files": agg.total_files,
            "bytes": agg.total_bytes,
            "safe_bytes": agg.safe_bytes,
            "confirm_bytes": agg.confirm_bytes,
            "protected_bytes": agg.protected_bytes,
        },
        "roots": roots,
        "by_agent": by_agent,
        "by_class": by_class,
        "by_rule": by_rule,
        "largest": largest,
    });

    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
}

fn ratio(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

/// 清理计划预览。执行前后共用，区别只在结尾的下一步提示。
pub fn render_plan(plan: &Plan) -> String {
    let mut out = String::new();

    out.push_str("清理计划（dry-run，尚未移动任何文件）\n");
    out.push_str(&format!("生成时间 {}\n", plan.generated_at));
    if !plan.running_agents.is_empty() {
        out.push_str(&format!(
            "检测到正在运行：{}（相关条目已跳过）\n",
            plan.running_agents.join(", ")
        ));
    }
    out.push_str(&format!(
        "\n将移动 {} 项，合计 {}\n",
        thousands(plan.items.len() as u64),
        human_bytes(plan.total_bytes())
    ));
    if !plan.skipped.is_empty() {
        out.push_str(&format!(
            "另有 {} 项被跳过 / {}\n",
            thousands(plan.skipped.len() as u64),
            human_bytes(plan.skipped_bytes())
        ));
    }
    out.push('\n');

    if plan.is_empty() {
        out.push_str("没有可清理的条目。\n");
        return out;
    }

    // 按规则分布
    let mut rules: Vec<_> = plan.by_rule().into_iter().collect();
    rules.sort_by_key(|(_, (_, bytes))| std::cmp::Reverse(*bytes));
    let mut t = new_table(vec!["规则 ID", "文件数", "体积"]);
    for (rule, (files, bytes)) in rules.iter().take(12) {
        t.add_row(vec![
            truncate(rule, 36),
            thousands(*files),
            human_bytes(*bytes),
        ]);
    }
    out.push_str("按规则分布\n");
    out.push_str(&t.to_string());
    out.push_str("\n\n");

    // 明细
    let mut t = new_table(vec!["#", "路径", "体积", "类别", "风险"]);
    for item in plan.items.iter().take(15) {
        t.add_row(vec![
            item.index.to_string(),
            truncate(&item.path.to_string_lossy().replace('\\', "/"), 52),
            human_bytes(item.bytes),
            item.class.label().to_string(),
            item.risk.label().to_string(),
        ]);
    }
    out.push_str("明细（按体积取前 15 项）\n");
    out.push_str(&t.to_string());
    if plan.items.len() > 15 {
        out.push_str(&format!("\n（共 {} 项，其余省略）", plan.items.len()));
    }
    out.push_str("\n\n");

    // 跳过原因
    if !plan.skipped.is_empty() {
        let mut reasons: Vec<_> = plan.skip_reasons().into_iter().collect();
        reasons.sort_by_key(|(_, (_, bytes))| std::cmp::Reverse(*bytes));
        let mut t = new_table(vec!["跳过原因", "文件数", "体积"]);
        for (reason, (files, bytes)) in reasons {
            t.add_row(vec![
                truncate(&reason, 40),
                thousands(files),
                human_bytes(bytes),
            ]);
        }
        out.push_str("跳过原因\n");
        out.push_str(&t.to_string());
        out.push_str("\n\n");
    }

    out.push_str("下一步\n");
    out.push_str("  curator clean --apply        把以上条目移入隔离区（可还原）\n");
    out.push_str("  curator clean --apply --yes  跳过交互确认\n");
    out.push_str("  需连带处理\"需人工确认\"项时追加 --include-review\n");
    out
}

/// 执行结果。
pub fn render_apply(report: &ApplyReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "已移入隔离区：{} 项 / {}\n",
        thousands(report.moved as u64),
        human_bytes(report.bytes)
    ));
    out.push_str(&format!("批次号 {}\n", report.run_id));
    out.push_str(&format!("位置   {}\n", report.run_dir.display()));

    if !report.failures.is_empty() {
        out.push_str(&format!("\n{} 项失败（源文件未改动）：\n", report.failures.len()));
        for (path, err) in report.failures.iter().take(10) {
            out.push_str(&format!(
                "  - {} : {}\n",
                truncate(&path.to_string_lossy().replace('\\', "/"), 56),
                truncate(err, 40)
            ));
        }
        if report.failures.len() > 10 {
            out.push_str(&format!("  ... 其余 {} 项省略\n", report.failures.len() - 10));
        }
    }

    out.push_str(&format!(
        "\n还原：curator restore {}\n",
        report.run_id
    ));
    out
}

/// 还原结果。
pub fn render_restore(report: &RestoreReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "已还原：{} 项 / {}\n",
        thousands(report.restored as u64),
        human_bytes(report.restored_bytes)
    ));

    if !report.conflicts.is_empty() {
        out.push_str(&format!("\n{} 项冲突（未覆盖，文件仍在隔离区）：\n", report.conflicts.len()));
        for (path, reason) in report.conflicts.iter().take(10) {
            out.push_str(&format!(
                "  - {} : {}\n",
                truncate(&path.to_string_lossy().replace('\\', "/"), 56),
                reason
            ));
        }
        if report.conflicts.len() > 10 {
            out.push_str(&format!("  ... 其余 {} 项省略\n", report.conflicts.len() - 10));
        }
    }

    if !report.failures.is_empty() {
        out.push_str(&format!("\n{} 项失败：\n", report.failures.len()));
        for (path, err) in report.failures.iter().take(10) {
            out.push_str(&format!(
                "  - {} : {}\n",
                truncate(&path.to_string_lossy().replace('\\', "/"), 56),
                truncate(err, 40)
            ));
        }
    }
    out
}
