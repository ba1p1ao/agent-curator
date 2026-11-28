//! 本机 AI agent 探测。
//!
//! 规则库只覆盖"我们知道怎么分类的" agent；但用户机器上可能装着十几种。
//! 这个模块负责回答两个问题：
//! 1. 装了哪些？（命令行能跑 / 配置目录存在）
//! 2. 哪些装了但我们还没规则？（这些文件会被判成"未识别"，需要补规则）
//!
//! 判定手段按可靠性排序：
//! - 可执行文件在 PATH 里 + `--version` 能跑通 → 确定装了
//! - 配置目录存在 → 基本可以确定用过
//!
//! 两者都没有就不报，避免"看起来装了一堆"的噪音。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::rules::RuleLibrary;
use crate::util::home_dir;

/// 一个已知 agent 的探测配方。
struct Probe {
    id: &'static str,
    name: &'static str,
    /// 可执行文件名（不含扩展名）
    bins: &'static [&'static str],
    /// 配置目录，`~` 开头表示相对用户主目录
    dirs: &'static [&'static str],
    note: &'static str,
}

/// 已知 agent 清单。
///
/// 只收录确实存在、且有独立数据目录的产品；宁可漏也不要瞎报，
/// 因为"检测到了但没规则"会给用户制造无效焦虑。
const PROBES: &[Probe] = &[
    Probe {
        id: "claude-code",
        name: "Claude Code",
        bins: &["claude"],
        dirs: &["~/.claude"],
        note: "Anthropic 官方 CLI",
    },
    Probe {
        id: "codex",
        name: "Codex",
        bins: &["codex"],
        dirs: &["~/.codex"],
        note: "OpenAI 官方 CLI",
    },
    Probe {
        id: "workbuddy",
        name: "WorkBuddy",
        bins: &["workbuddy"],
        dirs: &["~/.workbuddy"],
        note: "国内桌面 agent",
    },
    Probe {
        id: "gemini-cli",
        name: "Gemini CLI",
        bins: &["gemini"],
        dirs: &["~/.gemini"],
        note: "Google 官方 CLI",
    },
    Probe {
        id: "qwen-code",
        name: "Qwen Code",
        bins: &["qwen"],
        dirs: &["~/.qwen"],
        note: "通义千问 CLI",
    },
    Probe {
        id: "opencode",
        name: "OpenCode",
        bins: &["opencode"],
        dirs: &["~/.opencode", "~/.local/share/opencode"],
        note: "开源终端 agent",
    },
    Probe {
        id: "aider",
        name: "Aider",
        bins: &["aider"],
        dirs: &["~/.aider", "~/.aider.conf.yml"],
        note: "老牌结对编程 CLI",
    },
    Probe {
        id: "cursor",
        name: "Cursor",
        bins: &["cursor"],
        dirs: &["~/.cursor", "~/AppData/Roaming/Cursor"],
        note: "AI 编辑器",
    },
    Probe {
        id: "windsurf",
        name: "Windsurf",
        bins: &["windsurf"],
        dirs: &["~/.windsurf", "~/.codeium/windsurf"],
        note: "Codeium 系编辑器",
    },
    Probe {
        id: "continue",
        name: "Continue",
        bins: &["cn"],
        dirs: &["~/.continue"],
        note: "IDE 插件",
    },
    Probe {
        id: "goose",
        name: "Goose",
        bins: &["goose"],
        dirs: &["~/.config/goose"],
        note: "Block 开源 agent",
    },
    Probe {
        id: "amp",
        name: "Amp",
        bins: &["amp"],
        dirs: &["~/.amp"],
        note: "Sourcegraph CLI",
    },
    Probe {
        id: "crush",
        name: "Crush",
        bins: &["crush"],
        dirs: &["~/.crush", "~/.config/crush"],
        note: "Charm 终端 agent",
    },
    Probe {
        id: "trae",
        name: "Trae",
        bins: &["trae"],
        // 国际版 .trae / 国内版 .trae-cn 都算
        dirs: &["~/.trae", "~/.trae-cn", "~/AppData/Roaming/Trae CN"],
        note: "字节 AI 编辑器",
    },
    Probe {
        id: "kiro",
        name: "Kiro",
        bins: &["kiro"],
        dirs: &["~/.kiro"],
        note: "AWS AI 编辑器",
    },
    // ---------- 国产 agent ----------
    // 路径来自本机实际探测：命令行基本都不在 PATH 里，
    // 靠配置目录判定安装状态，比猜命令可靠。
    Probe {
        id: "kimi",
        name: "Kimi",
        bins: &["kimi"],
        dirs: &[
            "~/.kimi-work",
            "~/.kimi-webbridge",
            "~/AppData/Roaming/kimi-desktop",
        ],
        note: "月之暗面 Kimi 桌面端",
    },
    Probe {
        id: "dsh",
        name: "DSH",
        bins: &["dsh"],
        dirs: &["~/.dsh", "~/AppData/Roaming/DSH Desktop"],
        note: "DeepSeek 桌面 agent（目录内含 llm-deepseek / openai-gateway）",
    },
    Probe {
        id: "qwen-office",
        name: "千问办公",
        bins: &[],
        dirs: &["~/.qwenworkcn", "~/AppData/Roaming/QwenWorkCN"],
        note: "阿里千问办公桌面端",
    },
    Probe {
        id: "qoder",
        name: "Qoder",
        bins: &["qoder"],
        dirs: &[
            "~/.qoder",
            "~/.qoder-cli",
            "~/.qoder-cn",
            "~/AppData/Roaming/Qoder",
        ],
        note: "阿里 AI IDE（projects / memory / qoder-knowledge）",
    },
    Probe {
        id: "zcode",
        name: "ZCode",
        bins: &["zcode"],
        dirs: &["~/.zcode", "~/AppData/Roaming/ZCode"],
        note: "带 skills / plugin-workspace 的编码 agent",
    },
    Probe {
        id: "marscode",
        name: "MarsCode",
        bins: &["marscode"],
        dirs: &["~/.marscode"],
        note: "豆包 MarsCode",
    },
    Probe {
        id: "codebuddy",
        name: "CodeBuddy",
        bins: &["codebuddy"],
        dirs: &["~/.codebuddy"],
        note: "腾讯 CodeBuddy",
    },
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedAgent {
    pub id: String,
    pub name: String,
    pub note: String,
    /// 在 PATH 里找到的可执行文件
    pub binary: Option<String>,
    /// `--version` 输出（取首行）。脚本类可执行文件不执行，因此可能为空
    pub version: Option<String>,
    /// 实际存在的配置目录
    pub dirs: Vec<String>,
    /// 规则库是否覆盖它
    pub has_rules: bool,
}

/// 探测所有已知 agent，只返回"确实装了"的。
pub fn detect_all(lib: &RuleLibrary) -> Vec<DetectedAgent> {
    let home = home_dir();
    let mut found = Vec::new();

    for probe in PROBES {
        let binary = probe
            .bins
            .iter()
            .find_map(|name| find_in_path(name))
            .map(|path| {
                // 版本交给 probe_version 判断：.exe 直接跑，
                // Windows 的 .cmd/.bat 由它自己决定要不要经 cmd 转发
                let version = probe_version(&path);
                (path, version)
            });

        let dirs: Vec<String> = probe
            .dirs
            .iter()
            .map(|raw| expand(raw, &home))
            .filter(|path| path.exists())
            .map(|path| path.display().to_string())
            .collect();

        if binary.is_none() && dirs.is_empty() {
            continue;
        }

        let (binary_path, version) = match binary {
            Some((path, version)) => (Some(path.display().to_string()), version),
            None => (None, None),
        };

        found.push(DetectedAgent {
            id: probe.id.to_string(),
            name: probe.name.to_string(),
            note: probe.note.to_string(),
            binary: binary_path,
            version,
            dirs,
            has_rules: lib.agents.iter().any(|(id, _)| id == probe.id),
        });
    }

    found
}

/// 规则库没覆盖、但本机确实装了的 agent。
pub fn uncovered(agents: &[DetectedAgent]) -> Vec<&DetectedAgent> {
    agents.iter().filter(|a| !a.has_rules).collect()
}

fn expand(raw: &str, home: &Path) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest.replace('/', std::path::MAIN_SEPARATOR_STR))
    } else if raw == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(raw)
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    // Windows 上 .exe 优先。**不能把无扩展名排在前面**：
    // npm 会在同目录同时生成 `claude`（bash 脚本）、`claude.cmd`、`claude.ps1`，
    // 无扩展名那个在 Windows 上根本无法执行，先命中它就读不到版本。
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };

    for dir in std::env::split_paths(&path_var) {
        for ext in exts {
            let candidate = dir.join(format!("{}{}", name, ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// 能否直接 spawn。Windows 上 .cmd/.bat 不能，必须经 cmd.exe。
///
/// 生产代码里只有 Windows 分支会用到它；非 Windows 平台仅由单元测试覆盖，
/// 因此这里允许 dead_code，否则 `-D warnings` 会在 Linux/macOS 上直接报错。
#[cfg_attr(not(windows), allow(dead_code))]
fn is_direct_executable(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        None => true,
        Some(ext) => ext.eq_ignore_ascii_case("exe"),
    }
}

/// 跑 `<bin> --version`，最多等 5 秒。读不到就返回 None，不报错。
/// Node 写的 CLI（Claude Code 等）冷启动接近 1 秒，超时设太短会误判成"读不到"。
#[cfg(windows)]
fn probe_version(bin: &Path) -> Option<String> {
    if is_direct_executable(bin) {
        let mut cmd = Command::new(bin);
        cmd.arg("--version");
        return run_capture(cmd);
    }

    // .cmd / .bat 必须借 cmd.exe 转发。只接受"干净"路径（无空格、无 shell 元字符），
    // 否则宁可不读版本，也不去拼接一条可能被重新解释的命令行。
    let raw = bin.to_string_lossy().to_string();
    let risky = raw.chars().any(|c| {
        c.is_whitespace() || matches!(c, '"' | '&' | '|' | '<' | '>' | '^' | '%' | '(' | ')')
    });
    if risky {
        return None;
    }

    let mut cmd = Command::new("cmd");
    cmd.args(["/C", &raw, "--version"]);
    run_capture(cmd)
}

#[cfg(not(windows))]
fn probe_version(bin: &Path) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--version");
    run_capture(cmd)
}

/// 统一的「起进程 → 限时等待 → 取首行」流程。
fn run_capture(mut cmd: Command) -> Option<String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }

    let output = child.wait_with_output().ok()?;
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .to_string();
    Some(if line.chars().count() > 48 {
        line.chars().take(48).collect::<String>() + "…"
    } else {
        line
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_ids_are_unique_and_named() {
        let mut seen = std::collections::HashSet::new();
        for probe in PROBES {
            assert!(!probe.id.is_empty());
            assert!(!probe.name.is_empty());
            assert!(seen.insert(probe.id), "重复的 agent id: {}", probe.id);
        }
    }

    #[test]
    fn home_relative_dirs_expand() {
        let home = PathBuf::from("/home/demo");
        assert_eq!(expand("~/.claude", &home), home.join(".claude"));
        assert_eq!(
            expand("~/AppData/Roaming/Cursor", &home),
            home.join("AppData").join("Roaming").join("Cursor")
        );
        assert_eq!(expand("/abs/path", &home), PathBuf::from("/abs/path"));
    }

    #[test]
    fn cmd_scripts_are_not_spawned() {
        assert!(!is_direct_executable(Path::new("C:/x/claude.cmd")));
        assert!(!is_direct_executable(Path::new("C:/x/claude.bat")));
        assert!(is_direct_executable(Path::new("C:/x/claude.exe")));
        assert!(is_direct_executable(Path::new("/usr/bin/claude")));
    }
}
