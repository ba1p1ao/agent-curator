//! 资产备份：把 skills / memory / 配置 打包成可迁移的 zip。
//!
//! 与「清理」是两条完全相反的产品线：
//! - 清理挑的是**大而无价值**的文件（缓存、日志），目标是不心疼；
//! - 备份挑的是**小而不可再生**的文件（技能、记忆、配置），目标是一个都不许丢。
//!
//! 每个条目记录 sha256，还原时逐条校验；zip 本身也有校验和，便于离线核对。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::model::{DataClass, Node};

pub const FORMAT_VERSION: u32 = 1;
pub const MANIFEST_NAME: &str = "agentcurator-manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format_version: u32,
    pub tool: String,
    pub created_at: String,
    pub host: String,
    /// 备份时扫描到的 agent 根目录
    pub roots: Vec<String>,
    pub entries: Vec<BackupEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub agent: String,
    /// 备份时的原始绝对路径
    pub original: PathBuf,
    /// zip 内的条目名
    pub stored: String,
    pub bytes: u64,
    pub mtime: Option<i64>,
    pub sha256: String,
    pub rule_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct BackupReport {
    pub path: PathBuf,
    pub entries: usize,
    pub source_bytes: u64,
    pub zip_bytes: u64,
    pub zip_sha256: String,
    /// 因属于可重建元数据而跳过的文件数与体积
    pub excluded_files: usize,
    pub excluded_bytes: u64,
    pub failures: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct BackupOptions {
    pub label: Option<String>,
    /// 已知的 agent 根目录，用来把包内路径裁成相对形式
    pub roots: Vec<String>,
    /// 是否把 .git / node_modules / 日志这类可重建内容也打进包
    pub include_noise: bool,
}

/// 判断某个文件是不是「可重建元数据」。
///
/// 默认排除的理由：`.git` 里的对象、`node_modules`、日志都能重新拉取或生成，
/// 打进备份包只会让包变大、还原变慢，还容易让人误以为"这些也得留着"。
pub fn is_noise(path: &Path) -> bool {
    const NOISE_DIRS: &[&str] = &[
        ".git",
        "node_modules",
        "__pycache__",
        ".venv",
        ".pnpm-store",
    ];

    for part in path.components() {
        if let std::path::Component::Normal(seg) = part {
            let seg = seg.to_string_lossy();
            if NOISE_DIRS.iter().any(|d| seg.eq_ignore_ascii_case(d)) {
                return true;
            }
        }
    }

    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name == ".DS_Store" || name == "Thumbs.db" {
            return true;
        }
    }

    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()),
        Some(ext) if ext == "log" || ext == "tmp" || ext == "pyc"
    )
}

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub entries: usize,
    pub ok: usize,
    pub mismatched: Vec<(String, String)>,
    pub missing: Vec<String>,
}

#[derive(Debug, Default)]
pub struct RestoreBackupReport {
    pub restored: usize,
    pub bytes: u64,
    pub skipped: Vec<(PathBuf, String)>,
    pub failed: Vec<(PathBuf, String)>,
}

/// 把资产类节点打包。只读源文件，不改动任何原件。
pub fn create(nodes: &[Node], out_dir: &Path, opts: &BackupOptions) -> Result<BackupReport> {
    let assets: Vec<&Node> = nodes
        .iter()
        .filter(|n| n.class == DataClass::Asset)
        .collect();
    if assets.is_empty() {
        bail!("没有可备份的资产条目（未发现 skills / memory / 配置类文件）");
    }

    fs::create_dir_all(out_dir)
        .with_context(|| format!("创建备份目录失败: {}", out_dir.display()))?;

    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let name = match opts.label.as_deref() {
        Some(l) if !l.is_empty() => format!("agentcurator-{}-{}.zip", sanitize(l), stamp),
        _ => format!("agentcurator-{}.zip", stamp),
    };
    let zip_path = out_dir.join(&name);

    let file = File::create(&zip_path)
        .with_context(|| format!("创建备份文件失败: {}", zip_path.display()))?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut report = BackupReport {
        path: zip_path.clone(),
        ..Default::default()
    };
    let mut entries: Vec<BackupEntry> = Vec::new();

    for node in assets {
        if !opts.include_noise && is_noise(&node.path) {
            report.excluded_files += 1;
            report.excluded_bytes += node.size;
            continue;
        }
        let stored = stored_name(&node.path, &opts.roots);
        let mut source = match File::open(&node.path) {
            Ok(f) => f,
            Err(e) => {
                report.failures.push((node.path.clone(), e.to_string()));
                continue;
            }
        };

        // 先算 hash 再写 zip：源文件只被读两次，比边写边算更容易保证一致
        let sha = match hash_reader(&mut source) {
            Ok(h) => h,
            Err(e) => {
                report.failures.push((node.path.clone(), e.to_string()));
                continue;
            }
        };
        // 关键：hash_reader 已把游标读到文件尾，必须回到起点，
        // 否则下面 io::copy 拷出来的是 0 字节空文件（曾经真的踩过）
        if let Err(e) = source.rewind() {
            report.failures.push((node.path.clone(), e.to_string()));
            continue;
        }

        if let Err(e) = writer.start_file(&stored, options) {
            report.failures.push((node.path.clone(), e.to_string()));
            continue;
        }
        if let Err(e) = io::copy(&mut source, &mut writer) {
            report.failures.push((node.path.clone(), e.to_string()));
            continue;
        }

        report.entries += 1;
        report.source_bytes += node.size;
        entries.push(BackupEntry {
            agent: node.agent.clone(),
            original: node.path.clone(),
            stored,
            bytes: node.size,
            mtime: node.mtime,
            sha256: sha,
            rule_id: node.rule_id.clone(),
        });
    }

    if entries.is_empty() {
        let _ = writer.finish();
        let _ = fs::remove_file(&zip_path);
        bail!("所有资产文件都读取失败，未生成备份");
    }

    let manifest = BackupManifest {
        format_version: FORMAT_VERSION,
        tool: format!("AgentCurator {}", env!("CARGO_PKG_VERSION")),
        created_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        host: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        roots: opts.roots.clone(),
        entries,
    };
    writer.start_file(MANIFEST_NAME, options)?;
    writer.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    writer.finish().context("写入 zip 收尾失败")?;

    report.zip_bytes = fs::metadata(&zip_path)?.len();
    report.zip_sha256 = hash_file(&zip_path)?;

    // 旁边留一份校验和，方便离线核对（sha256sum -c 可直接用）
    let sidecar = zip_path.with_extension("zip.sha256");
    let line = format!("{}  {}\n", report.zip_sha256, name);
    fs::write(&sidecar, line).ok();

    Ok(report)
}

/// 只校验不还原：逐条比对 manifest 里的 sha256。
pub fn verify(zip_path: &Path) -> Result<VerifyReport> {
    let (manifest, mut archive) = open_with_manifest(zip_path)?;
    let mut report = VerifyReport {
        entries: manifest.entries.len(),
        ..Default::default()
    };
    let index = build_index(&mut archive)?;

    for entry in &manifest.entries {
        let Some(&i) = index.get(&entry.stored) else {
            report.missing.push(entry.stored.clone());
            continue;
        };
        let mut file = archive.by_index(i)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        let actual = hex(&Sha256::digest(&buffer));
        if actual == entry.sha256 {
            report.ok += 1;
        } else {
            report.mismatched.push((
                entry.stored.clone(),
                format!("期望 {} 实际 {}", &entry.sha256[..12], &actual[..12]),
            ));
        }
    }
    Ok(report)
}

/// 还原备份。`to` 为空时回到原始绝对路径；给了 `to` 则把所有条目放到该目录下，
/// 并保留原始路径结构（便于换机或另盘检查）。
pub fn restore(zip_path: &Path, to: Option<&Path>, overwrite: bool) -> Result<RestoreBackupReport> {
    let (manifest, mut archive) = open_with_manifest(zip_path)?;
    let mut report = RestoreBackupReport::default();
    let index = build_index(&mut archive)?;

    for entry in &manifest.entries {
        let target = match to {
            Some(root) => root.join(safe_relative(&entry.stored)),
            None => entry.original.clone(),
        };

        if target.exists() && !overwrite {
            report
                .skipped
                .push((target.clone(), "目标已存在，未覆盖".to_string()));
            continue;
        }
        if let Some(parent) = target.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                report
                    .failed
                    .push((target.clone(), format!("创建目录失败: {}", e)));
                continue;
            }
        }

        let Some(&idx) = index.get(&entry.stored) else {
            report
                .failed
                .push((target.clone(), "备份包内缺少该条目".to_string()));
            continue;
        };

        let mut file = archive.by_index(idx)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;

        let actual = hex(&Sha256::digest(&buffer));
        if actual != entry.sha256 {
            report.failed.push((
                target.clone(),
                format!("校验和不符，已拒写（期望 {}）", &entry.sha256[..12]),
            ));
            continue;
        }

        match fs::write(&target, &buffer) {
            Ok(_) => {
                if let Some(secs) = entry.mtime {
                    if let Ok(stamp) = u64::try_from(secs) {
                        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(stamp);
                        let _ = fs::set_times(&target, fs::FileTimes::new().set_modified(t));
                    }
                }
                report.restored += 1;
                report.bytes += entry.bytes;
            }
            Err(e) => report.failed.push((target.clone(), e.to_string())),
        }
    }
    Ok(report)
}

/// 读取备份包的 manifest（供 CLI 展示摘要）。
pub fn read_manifest(zip_path: &Path) -> Result<BackupManifest> {
    let (manifest, _) = open_with_manifest(zip_path)?;
    Ok(manifest)
}

fn open_with_manifest(zip_path: &Path) -> Result<(BackupManifest, ZipArchive<File>)> {
    let file =
        File::open(zip_path).with_context(|| format!("打不开备份包: {}", zip_path.display()))?;
    let mut archive = ZipArchive::new(file).context("不是有效的 zip 文件")?;

    let mut raw = String::new();
    {
        let mut entry = archive
            .by_name(MANIFEST_NAME)
            .context("备份包内缺少 manifest，可能不是 AgentCurator 生成的")?;
        entry.read_to_string(&mut raw)?;
    }
    let manifest: BackupManifest = serde_json::from_str(&raw).context("manifest 解析失败")?;
    if manifest.format_version != FORMAT_VERSION {
        bail!(
            "不支持的备份格式版本 {}（本程序支持 {}）",
            manifest.format_version,
            FORMAT_VERSION
        );
    }
    Ok((manifest, archive))
}

/// zip 内条目名：以 agent 根目录为基准，只保留它下面那一段。
///
/// `C:/Users/x/.claude/skills/a.md` + 根 `C:/Users/x/.claude` -> `.claude/skills/a.md`
///
/// 为什么不用绝对路径：备份包是要拿去换机、传到别处的，里面塞
/// `C/Users/admin/...` 既暴露了本机用户名，解压时也要多套几层空目录。
fn stored_name(path: &Path, roots: &[String]) -> String {
    let full = path.to_string_lossy().replace('\\', "/");

    // 找最长的匹配根，避免 `~/.workbuddy` 被 `~/.work` 这类短前缀误伤
    let mut best: Option<String> = None;
    for root in roots {
        let root = root.replace('\\', "/");
        let root = root.trim_end_matches('/').to_string();
        let hit = full == root || full.starts_with(&(root.clone() + "/"));
        if hit && best.as_ref().map(|b| root.len() > b.len()).unwrap_or(true) {
            best = Some(root);
        }
    }

    if let Some(root) = best {
        let root_name = root.rsplit('/').next().unwrap_or("root").to_string();
        let rel = full[root.len()..].trim_start_matches('/');
        return if rel.is_empty() {
            root_name
        } else {
            format!("{}/{}", root_name, rel)
        };
    }

    // 没匹配到已知根（例如用了 --root 指向根目录之外）：退回老办法
    full.replace(':', "").trim_start_matches('/').to_string()
}

/// 把 zip 内条目名安全地变成相对路径，挡住 `..` 与盘符这类越界写法。
fn safe_relative(stored: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for seg in stored.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains(':') {
            continue;
        }
        out.push(seg);
    }
    out
}

fn sanitize(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// 建立「条目名 -> 索引」映射。ZipArchive 只能按索引取条目，
/// 反复线性查找会让校验在几千个文件时退化成 O(n^2)。
fn build_index(archive: &mut ZipArchive<File>) -> Result<HashMap<String, usize>> {
    let mut map = HashMap::new();
    for i in 0..archive.len() {
        let name = {
            let entry = archive.by_index(i)?;
            entry.name()?.to_string()
        };
        map.insert(name, i);
    }
    Ok(map)
}

fn hash_reader(reader: &mut File) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    hash_reader(&mut file)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, Risk};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "curator-backup-{}-{}-{}",
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

    fn opts(label: Option<&str>, roots: &[String]) -> BackupOptions {
        BackupOptions {
            label: label.map(|s| s.to_string()),
            roots: roots.to_vec(),
            include_noise: false,
        }
    }

    fn asset(path: &Path, size: u64) -> Node {
        Node {
            path: path.to_path_buf(),
            size,
            mtime: None,
            agent: "demo".into(),
            class: DataClass::Asset,
            risk: Some(Risk::Critical),
            action: Action::Protect,
            rule_id: Some("demo.asset.skills".into()),
            note: None,
        }
    }

    #[test]
    fn backup_then_restore_recovers_content() {
        let work = temp_dir("roundtrip");
        let src = work.join("src");
        fs::create_dir_all(&src).unwrap();
        let a = src.join("SKILL.md");
        fs::write(&a, b"# skill body").unwrap();

        let out = work.join("out");
        let report = create(&[asset(&a, 12)], &out, &opts(Some("test"), &[])).unwrap();
        assert_eq!(report.entries, 1);
        assert!(report.path.exists());
        assert_eq!(report.zip_sha256.len(), 64);
        assert!(report.path.with_extension("zip.sha256").exists());

        let v = verify(&report.path).unwrap();
        assert_eq!(v.ok, 1);
        assert!(v.mismatched.is_empty());

        // 删掉原文件后从备份还原
        fs::remove_file(&a).unwrap();
        let r = restore(&report.path, None, false).unwrap();
        assert_eq!(r.restored, 1);
        assert_eq!(fs::read(&a).unwrap(), b"# skill body");
    }

    #[test]
    fn restore_refuses_to_overwrite_by_default() {
        let work = temp_dir("overwrite");
        let src = work.join("src");
        fs::create_dir_all(&src).unwrap();
        let a = src.join("SKILL.md");
        fs::write(&a, b"v1").unwrap();

        let out = work.join("out");
        let report = create(&[asset(&a, 2)], &out, &opts(None, &[])).unwrap();
        fs::write(&a, b"v2-different").unwrap();

        let r = restore(&report.path, None, false).unwrap();
        assert_eq!(r.restored, 0);
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(fs::read(&a).unwrap(), b"v2-different");
    }

    #[test]
    fn restore_with_to_dir_rebuilds_structure() {
        let work = temp_dir("todir");
        let src = work.join("src");
        fs::create_dir_all(&src).unwrap();
        let a = src.join("nested").join("x.md");
        fs::create_dir_all(a.parent().unwrap()).unwrap();
        fs::write(&a, b"data").unwrap();

        let out = work.join("out");
        let report = create(&[asset(&a, 4)], &out, &opts(None, &[])).unwrap();

        let target = work.join("restored");
        let r = restore(&report.path, Some(&target), false).unwrap();
        assert_eq!(r.restored, 1);
        // 结构应保留在 to 目录下
        let found = walk(&target);
        assert_eq!(found.len(), 1);
        assert!(found[0].to_string_lossy().ends_with("x.md"));
    }

    #[test]
    fn stored_paths_are_relative_to_agent_root() {
        let work = temp_dir("relative");
        let root = work.join(".claude");
        let skills = root.join("skills");
        fs::create_dir_all(&skills).unwrap();
        let file = skills.join("a.md");
        fs::write(&file, b"x").unwrap();

        let out = work.join("out");
        let roots = vec![root.display().to_string()];
        let report = create(&[asset(&file, 1)], &out, &opts(None, &roots)).unwrap();

        let manifest = read_manifest(&report.path).unwrap();
        assert_eq!(
            manifest.entries[0].stored, ".claude/skills/a.md",
            "备份包内不该出现本机绝对路径"
        );

        // 还原到别处时，应当直接展开成 .claude/skills/a.md
        let dest = work.join("dest");
        restore(&report.path, Some(&dest), false).unwrap();
        assert!(dest.join(".claude").join("skills").join("a.md").exists());
    }

    #[test]
    fn git_and_node_modules_are_excluded_by_default() {
        let work = temp_dir("noise");
        let root = work.join(".claude");
        let git = root.join("plugins").join("demo").join(".git");
        let nm = root.join("plugins").join("demo").join("node_modules");
        fs::create_dir_all(&git).unwrap();
        fs::create_dir_all(&nm).unwrap();

        let keep = root.join("skills").join("a.md");
        fs::create_dir_all(keep.parent().unwrap()).unwrap();
        fs::write(&keep, b"keep").unwrap();

        let obj = git.join("config");
        fs::write(&obj, b"git").unwrap();
        let dep = nm.join("index.js");
        fs::write(&dep, b"js").unwrap();

        let nodes = vec![asset(&keep, 4), asset(&obj, 3), asset(&dep, 2)];
        let report = create(&nodes, &work.join("out"), &opts(None, &[])).unwrap();

        assert_eq!(report.entries, 1, "只应保留 skills/a.md");
        assert_eq!(report.excluded_files, 2);
        assert_eq!(report.excluded_bytes, 5);

        let manifest = read_manifest(&report.path).unwrap();
        assert!(manifest.entries.iter().all(|e| !e.stored.contains(".git")));
        assert!(manifest
            .entries
            .iter()
            .all(|e| !e.stored.contains("node_modules")));
    }

    #[test]
    fn noise_can_be_included_on_demand() {
        let work = temp_dir("noise-on");
        let git = work.join(".claude").join(".git");
        fs::create_dir_all(&git).unwrap();
        let obj = git.join("config");
        fs::write(&obj, b"git").unwrap();

        let mut o = opts(None, &[]);
        o.include_noise = true;
        let report = create(&[asset(&obj, 3)], &work.join("out"), &o).unwrap();
        assert_eq!(report.entries, 1);
        assert_eq!(report.excluded_files, 0);
    }

    #[test]
    fn non_asset_nodes_are_ignored() {
        let work = temp_dir("filter");
        let f = work.join("cache.bin");
        fs::write(&f, b"x").unwrap();
        let mut node = asset(&f, 1);
        node.class = DataClass::Ephemeral;

        let err = create(&[node], &work.join("out"), &opts(None, &[])).unwrap_err();
        assert!(err.to_string().contains("没有可备份的资产"));
    }

    #[test]
    fn tampered_entry_is_rejected_on_restore() {
        let work = temp_dir("tamper");
        let src = work.join("src");
        fs::create_dir_all(&src).unwrap();
        let a = src.join("a.md");
        fs::write(&a, b"hello").unwrap();
        let report = create(&[asset(&a, 5)], &work.join("out"), &opts(None, &[])).unwrap();

        // 直接把 manifest 里的 hash 改坏，模拟包被篡改
        let (mut manifest, _) = open_with_manifest(&report.path).unwrap();
        manifest.entries[0].sha256 = "00".repeat(32);
        let broken = work.join("out").join("broken.zip");
        rewrite_manifest(&report.path, &broken, &manifest);

        fs::remove_file(&a).unwrap();
        let r = restore(&broken, None, false).unwrap();
        assert_eq!(r.restored, 0);
        assert_eq!(r.failed.len(), 1);
        assert!(!a.exists(), "校验失败时绝不能落盘");
    }

    fn rewrite_manifest(src: &Path, dst: &Path, manifest: &BackupManifest) {
        let mut archive = ZipArchive::new(File::open(src).unwrap()).unwrap();
        let target = File::create(dst).unwrap();
        let mut writer = ZipWriter::new(target);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).unwrap();
            let name = entry.name().unwrap().to_string();
            if name == MANIFEST_NAME {
                continue;
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).unwrap();
            writer.start_file(name, opts).unwrap();
            writer.write_all(&buf).unwrap();
        }
        writer.start_file(MANIFEST_NAME, opts).unwrap();
        writer
            .write_all(serde_json::to_string_pretty(manifest).unwrap().as_bytes())
            .unwrap();
        writer.finish().unwrap();
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    out.extend(walk(&p));
                } else {
                    out.push(p);
                }
            }
        }
        out
    }
}
