//! 规则库：把 YAML 编译成一张全局 GlobSet，并提供「最保守优先」的裁决。
//!
//! 三条硬规则（改动前请先读）：
//! 1. 裁决只看 `Action::priority`，与规则在文件中的先后顺序无关；
//! 2. 多条命中时取最保守结论（priority → risk → class → 模式长度）；
//! 3. 无任何命中 → `unknown`，永不进入清理计划。

pub mod schema;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::model::{Action, DataClass, Risk};
use crate::util::{expand_tilde, home_dir, normalize_path};
use schema::{AgentRulesFile, RuleDef};

pub const SCHEMA_VERSION: u32 = 1;

/// 编译后的一条规则。
#[derive(Debug, Clone)]
pub struct RuleEntry {
    pub agent: String,
    pub id: String,
    pub class: DataClass,
    pub risk: Risk,
    pub action: Action,
    pub note: Option<String>,
    pub pattern: String,
    pub mtime_older_than_days: Option<u64>,
}

/// 展开 `~` 之后的扫描根。
#[derive(Debug, Clone)]
pub struct ResolvedRoot {
    pub agent: String,
    pub id: String,
    pub path: PathBuf,
    pub probe: Vec<String>,
    pub note: Option<String>,
}

#[derive(Debug)]
pub struct RuleLibrary {
    set: GlobSet,
    entries: Vec<RuleEntry>,
    pub sources: Vec<PathBuf>,
    /// (agent, display_name)
    pub agents: Vec<(String, String)>,
    pub roots: Vec<ResolvedRoot>,
    /// 非致命问题，例如未声明 class 的规则
    pub warnings: Vec<String>,
}

impl RuleLibrary {
    /// 加载一个目录下的全部 `.yaml` / `.yml`，按文件名排序保证结果稳定。
    pub fn load_dir(dir: &Path) -> Result<Self> {
        if !dir.is_dir() {
            bail!("规则目录不存在: {}", dir.display());
        }
        let mut files: Vec<PathBuf> = fs::read_dir(dir)
            .with_context(|| format!("无法读取规则目录: {}", dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter(|p| {
                matches!(
                    p.extension().and_then(|s| s.to_str()),
                    Some("yaml") | Some("yml")
                )
            })
            .collect();
        files.sort();

        if files.is_empty() {
            bail!("规则目录 {} 下没有任何 .yaml 文件", dir.display());
        }

        let mut docs = Vec::with_capacity(files.len());
        for file in &files {
            let text = fs::read_to_string(file)
                .with_context(|| format!("读取规则文件失败: {}", file.display()))?;
            let doc: AgentRulesFile = serde_yaml::from_str(&text)
                .with_context(|| format!("解析 YAML 失败: {}", file.display()))?;
            docs.push((file.clone(), doc));
        }
        Self::from_docs(docs)
    }

    fn from_docs(docs: Vec<(PathBuf, AgentRulesFile)>) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        let mut entries: Vec<RuleEntry> = Vec::new();
        let mut sources: Vec<PathBuf> = Vec::new();
        let mut agents: Vec<(String, String)> = Vec::new();
        let mut roots: Vec<ResolvedRoot> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();

        for (file, doc) in docs {
            sources.push(file.clone());

            if doc.schema_version != SCHEMA_VERSION {
                bail!(
                    "{}: schema_version 必须为 {}，实际为 {}",
                    file.display(),
                    SCHEMA_VERSION,
                    doc.schema_version
                );
            }
            if doc.agent.trim().is_empty() {
                bail!("{}: agent 字段不能为空", file.display());
            }
            agents.push((doc.agent.clone(), doc.display_name.clone()));

            for root in &doc.roots {
                if root.path.trim().is_empty() {
                    bail!("{}: 根目录 {} 的 path 为空", file.display(), root.id);
                }
                roots.push(ResolvedRoot {
                    agent: doc.agent.clone(),
                    id: root.id.clone(),
                    path: expand_tilde(&root.path),
                    probe: root.probe.clone(),
                    note: root.note.clone(),
                });
            }

            for rule in &doc.rules {
                if !seen_ids.insert(rule.id.clone()) {
                    bail!("{}: 规则 id 重复: {}", file.display(), rule.id);
                }
                if rule.class.is_none() && rule.action.is_none() {
                    bail!(
                        "{}: 规则 {} 至少要声明 class 或 action，否则无法判定处置方式",
                        file.display(),
                        rule.id
                    );
                }
                if rule.class.is_none() {
                    warnings.push(format!(
                        "{}: 规则 {} 未声明 class，将按 unknown 处理（默认保留）",
                        file.display(),
                        rule.id
                    ));
                }

                let pattern = build_glob_pattern(&rule.path)
                    .with_context(|| format!("{}: 规则 {}", file.display(), rule.id))?;

                let glob = GlobBuilder::new(&pattern)
                    .literal_separator(true)
                    .case_insensitive(cfg!(windows))
                    .backslash_escape(false)
                    .build()
                    .with_context(|| {
                        format!(
                            "{}: 规则 {} 的 glob 模式非法: {}",
                            file.display(),
                            rule.id,
                            pattern
                        )
                    })?;

                builder.add(glob);
                entries.push(compile_entry(&doc.agent, rule, pattern));
            }
        }

        let set = builder.build().context("构建 glob 集合失败")?;
        if entries.is_empty() {
            bail!("规则库为空：所有规则文件都没有有效的 rules 条目");
        }

        Ok(Self {
            set,
            entries,
            sources,
            agents,
            roots,
            warnings,
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[RuleEntry] {
        &self.entries
    }

    /// 对一条已归一化的绝对路径做裁决，返回最保守的那条规则。
    ///
    /// `now_secs` 由调用方传入并在整轮扫描中复用，避免每条文件都取一次系统时间。
    pub fn match_best(
        &self,
        abs_path: &str,
        mtime_secs: Option<i64>,
        now_secs: i64,
    ) -> Option<&RuleEntry> {
        let hit_indices = self.set.matches(abs_path);
        if hit_indices.is_empty() {
            return None;
        }

        let mut best: Option<&RuleEntry> = None;
        for idx in hit_indices {
            let entry = &self.entries[idx];

            // 时间门槛不满足的规则直接放弃，让更宽松的规则接手
            if let Some(days) = entry.mtime_older_than_days {
                let cutoff = now_secs - (days as i64) * 86_400;
                match mtime_secs {
                    Some(m) if m <= cutoff => {}
                    _ => continue,
                }
            }

            best = match best {
                None => Some(entry),
                Some(current) => {
                    if rank(entry) > rank(current) {
                        Some(entry)
                    } else {
                        Some(current)
                    }
                }
            };
        }
        best
    }
}

fn compile_entry(agent: &str, rule: &RuleDef, pattern: String) -> RuleEntry {
    RuleEntry {
        agent: agent.to_string(),
        id: rule.id.clone(),
        class: rule.effective_class(),
        risk: rule.effective_risk(),
        action: rule.effective_action(),
        note: rule.note.clone(),
        pattern,
        mtime_older_than_days: rule.mtime_older_than_days,
    }
}

/// 保守度排序键：动作优先级 → 风险等级 → 类别保守度 → 模式越具体越优先。
fn rank(entry: &RuleEntry) -> (u8, u8, u8, usize) {
    (
        entry.action.priority(),
        entry.risk as u8,
        entry.class.conservativeness(),
        entry.pattern.len(),
    )
}

/// 把规则里的 `~` 替换成真实主目录，并对主目录部分做 glob 转义
/// （用户名里可能包含 `[`、`]` 之类会被当成字符类的字符）。
fn build_glob_pattern(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("path 不能为空");
    }
    if let Some(rest) = raw.strip_prefix('~') {
        let home = normalize_path(&home_dir());
        return Ok(format!("{}{}", globset::escape(&home), rest));
    }
    Ok(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib_from_yaml(yaml: &str) -> RuleLibrary {
        let doc: AgentRulesFile = serde_yaml::from_str(yaml).expect("yaml 应可解析");
        RuleLibrary::from_docs(vec![(PathBuf::from("test.yaml"), doc)]).expect("规则应可编译")
    }

    const BASE: &str = r#"
schema_version: 1
agent: demo
display_name: Demo
roots:
  - id: home
    path: "~/.demo"
rules:
  - id: demo.assets
    path: "~/.demo/skills/**"
    class: asset
    risk: critical
    action: protect
  - id: demo.cache
    path: "~/.demo/cache/**"
    class: ephemeral
    risk: low
    action: clean
"#;

    #[test]
    fn protect_wins_over_clean_regardless_of_order() {
        let lib = lib_from_yaml(BASE);
        let home = normalize_path(&home_dir());
        let hit = lib
            .match_best(&format!("{}/.demo/skills/a/b.md", home), None, 0)
            .expect("应命中");
        assert_eq!(hit.action, Action::Protect);
        assert_eq!(hit.class, DataClass::Asset);
    }

    #[test]
    fn glob_star_does_not_cross_directories() {
        let lib = lib_from_yaml(BASE);
        let home = normalize_path(&home_dir());
        // `.demo/cache/x` 命中，`.demo/cachex` 不应命中
        assert!(lib
            .match_best(&format!("{}/.demo/cache/x.bin", home), None, 0)
            .is_some());
        assert!(lib
            .match_best(&format!("{}/.demo/cachex/x.bin", home), None, 0)
            .is_none());
    }

    #[test]
    fn unmatched_path_returns_none() {
        let lib = lib_from_yaml(BASE);
        let home = normalize_path(&home_dir());
        assert!(lib
            .match_best(&format!("{}/.demo/whatever/x", home), None, 0)
            .is_none());
    }

    #[test]
    fn schema_version_mismatch_is_rejected() {
        let bad = BASE.replace("schema_version: 1", "schema_version: 99");
        let doc: AgentRulesFile = serde_yaml::from_str(&bad).unwrap();
        let err = RuleLibrary::from_docs(vec![(PathBuf::from("bad.yaml"), doc)]).unwrap_err();
        assert!(err.to_string().contains("schema_version"));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let bad = BASE.replace("class: asset", "klass: asset");
        assert!(serde_yaml::from_str::<AgentRulesFile>(&bad).is_err());
    }
}
