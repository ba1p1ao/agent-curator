//! 内置规则库。
//!
//! 编译期把 `rules/*.yaml` 嵌进二进制。原因很实际：
//! 用户从 Releases 下载到的只有一个 exe，旁边的 `rules/` 目录很容易丢，
//! 一旦丢了整个产品就废了（所有文件都变成"未识别"）。
//!
//! 优先级：外部 `rules/` 目录 > 内置。
//! 这样开发时改 YAML 不用重编译，发版时又不会因为漏打包而失效。

/// (文件名, 内容)
pub const SOURCES: &[(&str, &str)] = &[
    (
        "claude-code.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/claude-code.yaml"
        )),
    ),
    (
        "codex.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/codex.yaml"
        )),
    ),
    (
        "workbuddy.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/workbuddy.yaml"
        )),
    ),
    (
        "gemini-cli.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/gemini-cli.yaml"
        )),
    ),
    (
        "kiro.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/kiro.yaml"
        )),
    ),
    (
        "opencode.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/opencode.yaml"
        )),
    ),
    (
        "continue.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/continue.yaml"
        )),
    ),
    (
        "qwen-office.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/qwen-office.yaml"
        )),
    ),
    (
        "dsh.yaml",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../rules/dsh.yaml")),
    ),
    (
        "marscode.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/marscode.yaml"
        )),
    ),
    (
        "trae.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/trae.yaml"
        )),
    ),
    (
        "zcode.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/zcode.yaml"
        )),
    ),
    (
        "qoder.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/qoder.yaml"
        )),
    ),
    (
        "kimi.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/kimi.yaml"
        )),
    ),
    (
        "codebuddy.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/codebuddy.yaml"
        )),
    ),
    (
        "generic.yaml",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/generic.yaml"
        )),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RuleLibrary;

    #[test]
    fn builtin_rules_compile_and_cover_known_agents() {
        let lib = RuleLibrary::load_builtin().expect("内置规则必须可用");
        assert!(lib.len() > 50, "内置规则数量异常: {}", lib.len());
        // 每新增一份 rules/*.yaml 都必须登记到 SOURCES，否则发行版里会缺规则。
        // 这条断言就是防止"加了文件忘了注册"。
        assert_eq!(SOURCES.len(), 16, "SOURCES 与 rules/ 目录下的文件数不一致");
        for agent in [
            "claude-code",
            "codex",
            "workbuddy",
            "gemini-cli",
            "kiro",
            "opencode",
            "continue",
            "qwen-office",
            "dsh",
            "marscode",
            "trae",
            "zcode",
            "qoder",
            "kimi",
            "codebuddy",
        ] {
            assert!(
                lib.agents.iter().any(|(id, _)| id == agent),
                "内置规则缺少 agent: {}",
                agent
            );
        }
    }

    #[test]
    fn every_source_file_is_non_empty() {
        for (name, text) in SOURCES {
            assert!(!text.trim().is_empty(), "内置规则文件为空: {}", name);
        }
    }
}
