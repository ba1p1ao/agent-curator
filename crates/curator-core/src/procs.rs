//! 进程检测：判断某个 agent 当前是否正在运行。
//!
//! 为什么必须有这一层：agent 运行时持续写 `sessions/`、`logs/`、
//! `*.sqlite`（还带 `-wal`/`-shm`）。此时删文件轻则会话损坏，重则索引库报废。
//! 因此只要命中进程名，该 agent 下的**所有**条目默认跳过，
//! 用户必须显式传 `--include-running` 才放行。

use std::collections::BTreeSet;

use anyhow::{anyhow, Result};
use sysinfo::System;

use crate::rules::RuleLibrary;

/// 返回当前正在运行的 agent 标识列表。
///
/// 读取失败时返回 `Err` 而不是空列表：把"读不到"当成"没有运行"
/// 会让调用方在 agent 正在写盘时误删数据，必须由上层决定怎么处理。
pub fn detect_running(lib: &RuleLibrary) -> Result<Vec<String>> {
    let mut processes: BTreeSet<String> = BTreeSet::new();
    let system = System::new_all().map_err(|e| anyhow!("读取进程表失败: {}", e))?;
    for process in system.processes().values() {
        let name = process.name().to_string_lossy().to_ascii_lowercase();
        if !name.is_empty() {
            processes.insert(name);
        }
    }
    if processes.is_empty() {
        return Ok(Vec::new());
    }
    Ok(match_agents(lib, &processes))
}

/// 便于单测：不依赖真实进程表。
pub fn match_agents(lib: &RuleLibrary, process_names: &BTreeSet<String>) -> Vec<String> {
    let mut running = Vec::new();
    for (agent, wanted) in &lib.processes {
        if wanted
            .iter()
            .any(|w| process_names.contains(w) || process_names.contains(&format!("{}.exe", w)))
        {
            running.push(agent.clone());
        }
    }
    running
}

#[cfg(test)]
mod tests {
    use super::match_agents;
    use crate::rules::RuleLibrary;
    use std::path::PathBuf;

    fn lib() -> RuleLibrary {
        let yaml = r#"
schema_version: 1
agent: demo
display_name: Demo
processes: ["demo-agent"]
rules:
  - id: demo.cache
    path: "~/.demo/cache/**"
    class: ephemeral
    action: clean
"#;
        let doc = serde_yaml::from_str(yaml).unwrap();
        RuleLibrary::from_docs(vec![(PathBuf::from("demo.yaml"), doc)]).unwrap()
    }

    #[test]
    fn detects_running_agent_by_process_name() {
        let lib = lib();
        let mut names = std::collections::BTreeSet::new();
        names.insert("demo-agent.exe".to_string());
        assert_eq!(match_agents(&lib, &names), vec!["demo".to_string()]);
    }

    #[test]
    fn no_match_when_process_absent() {
        let lib = lib();
        let names = std::collections::BTreeSet::new();
        assert!(match_agents(&lib, &names).is_empty());
    }
}
