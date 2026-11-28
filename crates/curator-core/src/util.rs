//! 路径与数值格式化工具。所有路径统一成 `C:/Users/x/y` 形式再参与规则匹配。

use std::path::{Path, PathBuf};

/// 统一成正斜杠绝对路径，并去掉 Windows 的 `\\?\` 前缀与末尾分隔符。
/// 统一成正斜杠、去掉 Windows 扩展长度前缀、去掉尾部斜杠。
///
/// 这是扫描热路径上每个文件都要走一次的函数，所以刻意只做**一次分配**：
/// 老实现先 `replace` 再 `to_string`，一条路径可能分配 2-3 次，
/// 十万级文件时这部分开销相当可观。
pub fn normalize_path(p: &Path) -> String {
    let lossy = p.to_string_lossy();
    let src = lossy.as_ref();
    // 扩展长度前缀两种写法都可能出现（`\\?\` 或已转义过的 `//?/`）
    let src = src
        .strip_prefix(r"\\?\")
        .or_else(|| src.strip_prefix("//?/"))
        .unwrap_or(src);

    let mut out = String::with_capacity(src.len() + 1);
    for ch in src.chars() {
        out.push(if ch == '\\' { '/' } else { ch });
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// 用户主目录。优先 `dirs`，回退 `USERPROFILE`。
pub fn home_dir() -> PathBuf {
    dirs::home_dir()
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 展开规则与命令行里的 `~`。
pub fn expand_tilde(raw: &str) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    PathBuf::from(raw)
}

/// 人类可读体积。
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// 千分位文件数，避免报表里出现 165344 这种难读的数字。
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 百分比。
pub fn pct(part: u64, total: u64) -> String {
    if total == 0 {
        return "-".to_string();
    }
    format!("{:.1}%", part as f64 * 100.0 / total as f64)
}

/// 按字符（不是字节）截断，中英混排不会截出半个字。
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_formats_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(165344), "165,344");
    }

    #[test]
    fn pct_handles_zero_total() {
        assert_eq!(pct(10, 0), "-");
        assert_eq!(pct(1, 4), "25.0%");
    }
}
