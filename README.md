# AgentCurator · 司库

> 给 AI agent 留下的文件做「清产核资」：分清哪些是要长期保管的资产、哪些是可以放心丢掉的痕迹。

`curator` 的本义是**典藏管理者**——给藏品编目、分类，决定哪些入库、哪些出库。
这个工具做的正是这件事，而且**保管和清理同等重要**，不做只删不管的清理器。

> **当前阶段：M3**
> 支持只读体检（`scan`）、**可还原的清理**（`clean` / `restore`）、
> **可校验的资产备份**（`backup` / `backup-verify` / `backup-restore`）。
> 清理默认 dry-run，只有显式 `--apply` 才会移动文件，且一律进隔离区，不直接删除。

## 为什么不是普通的磁盘清理工具

同一批目录里混着五种可删性完全不同的东西：

| 类别 | 典型内容 | 本机体量 | 处置 |
|---|---|---|---|
| 用户资产 | `skills/`、`agents/`、`memory/`、`CLAUDE.md`、配置、运行时 | 1.6 GB | 备份，禁止清理 |
| 用户数据 | `workspace/`、`projects/`、`blobs/`、上传附件 | 1.8 GB | 逐项人工确认 |
| 可重建垃圾 | `cache/`、`logs/`、`sessions/`、`*-wal`、截图 | 755 MB | 可安全释放 |
| 项目产物 | `dist/`、`node_modules/`、`target/` | 25 MB | 可由源码重建 |
| 未识别 | 无规则命中 | 51 MB | 默认保留 |

WizTree 之类的工具只懂"大"，CCleaner 只懂"缓存"，它们都不知道
`~/.workbuddy/skills` 比 `~/.workbuddy/cache` 重要一万倍。
**这个项目的护城河是规则库，不是删除逻辑。**

## 快速开始

```bash
cargo build --release

# 只读体检
./target/release/curator scan --md reports/scan.md --json reports/scan.json

# 清理：默认只出计划，不动文件
./target/release/curator clean

# 确认无误后执行（移入隔离区，可还原）
./target/release/curator clean --apply

# 反悔
./target/release/curator restore --last

# 备份资产：打包 skills / memory / 配置
./target/release/curator backup --label before-upgrade

# 校验备份包（逐条比对 sha256）
./target/release/curator backup-verify ~/curator-backups/agentcurator-xxx.zip

# 换机还原到指定目录
./target/release/curator backup-restore ~/curator-backups/agentcurator-xxx.zip --to D:/restore
```

## 清理与还原

`clean` 的完整链路是 **扫描 → 计划 → 人工确认 → 隔离区**，每一环都可观察：

```
$ curator clean
清理计划（dry-run，尚未移动任何文件）
将移动 3 项，合计 768.0 KB
按规则分布 / 明细 / 跳过原因 ...
下一步
  curator clean --apply        把以上条目移入隔离区（可还原）
  curator clean --apply --yes  跳过交互确认
```

**隔离区结构**（默认 `~/.curator-bin`，可用 `--bin` 或 `CURATOR_BIN_DIR` 改）：

```
~/.curator-bin/runs/<run_id>/
├── manifest.json      批次清单：原路径、隔离路径、体积、mtime、规则、风险
├── journal.jsonl      逐条追加的流水，崩溃后可据此恢复出真实状态
└── files/00001/…      被移走的文件，序号目录避免长路径问题
```

**三条安全约束**（代码里强制，不靠自觉）：

1. 同卷用 `rename`（原子、保留 mtime）；跨卷用「复制 → 校验字节数 → 删源」，
   校验不通过就回滚副本，绝不出现"删了源但副本不完整"。
2. 还原**不覆盖**。原路径已有文件就记为冲突并跳过，把文件留在隔离区等你处理。
3. 隔离区不能位于待清理路径内部，否则直接中止（防止自我吞噬）。

**进程检测**：`clean` 前会枚举进程名，命中 `rules/*.yaml` 里声明的
`processes` 时，该 agent 的条目全部跳过。因为 agent 运行时持续写
`sessions/`、`logs/`、`*.sqlite`（还带 `-wal`/`-shm`），此时删除轻则会话损坏，
重则索引库报废。确实要清就退出 agent，或显式加 `--include-running`。

## 资产备份

清理和备份是**两条方向相反的产品线**：清理挑"大而无价值"的，备份挑"小而不可再生"的，
两份清单几乎不重叠。所以它们各自独立成命令，不共用一个按钮。

```
$ curator backup --agent claude-code --label verify
备份完成：487 项 / 5.4 MB → 3.4 MB
压缩比   38.0%
备份包   ~/curator-backups/agentcurator-verify-20261127-182901.zip
校验和   8e90f983c80cbfc1
```

备份包结构：

```
agentcurator-<label>-<时间>.zip
├── agentcurator-manifest.json   每个文件的原始路径、体积、mtime、sha256
└── C/Users/.../skills/...       按原始路径展开（去掉盘符冒号，保证跨平台）
agentcurator-...zip.sha256       整包校验和，`sha256sum -c` 可直接用
```

三条约束：

1. **只读源文件**。备份过程不改动、不移动任何原件。
2. **逐条校验**。还原时比对 manifest 里的 sha256，不符就拒写，绝不把损坏内容盖回你的目录。
3. **默认不覆盖**。目标已存在就跳过并列出，要覆盖必须显式 `--overwrite`。

`backup` 只打包 `class: asset` 的条目——也就是 `skills/`、`agents/`、`memory/`、
`CLAUDE.md`、`settings.json` 这一类删了不可再生的东西。缓存和会话记录不在备份范围内。

## 工程结构

```
crates/
  curator-core/          核心库（无 CLI 依赖，后续可被 Tauri 直接引用）
    model.rs             领域词汇：DataClass / Risk / Action / Node
    rules/               规则编译与「最保守优先」裁决
    scanner.rs           只读并行扫描（jwalk）
    classifier.rs        路径 -> Node
    aggregate.rs         流式聚合，不保留全量节点
    plan.rs              候选集 -> 清理计划（纯函数，可单测）
    procs.rs             进程检测（sysinfo）
    quarantine.rs        隔离区：移动、manifest、还原、清理
    backup.rs            资产备份：zip + manifest.sha256 + 校验 + 还原
    report.rs            终端 / Markdown / JSON 报表
  curator-cli/           命令 `curator`：scan / clean / restore / bin / rules
rules/                   规则库（YAML，运行时可改，无需重新编译）
```

## 规则库

一条规则长这样：

```yaml
- id: codex.ephemeral.logs-db
  path: "~/.codex/logs*.sqlite*"
  class: ephemeral
  risk: high
  action: skip-if-locked
  note: 运行日志库；agent 运行时持有锁，必须整组跳过
```

三条**不可动摇**的裁决规则：

1. 优先级固定 `protect > skip-if-locked > review > clean`，与规则在文件里的顺序无关；
2. 多条规则同时命中时，取最保守的那条（动作 → 风险 → 类别 → 模式长度）；
3. 没有任何规则命中 → `unknown`，永远不进入清理计划。

修改规则后直接重新运行 `curator scan` 即可生效，不需要重新编译。
规则目录可用 `--rules <DIR>` 或环境变量 `CURATOR_RULES_DIR` 指定。

## 安全边界

- `scan` 全程只读元数据（size / mtime），不读内容、不做 hash；
- 不跟随符号链接与目录联接，避免体积重复统计和越界枚举；
- 无规则命中的路径一律按"保留"处理；
- `action: protect` 的条目（skills / memory / 配置 / 运行时）永远不会进入计划；
- 符号链接在移动阶段二次拦截；
- 审计留痕：每个批次的 manifest + journal 都在，可回查"什么时候移走了什么"。

## 路线图

| 阶段 | 内容 |
|---|---|
| M1 ✅ | 只读扫描 + 规则库 + 体积归类 + 报告导出 |
| M2 ✅ | 清理计划、dry-run 清单、隔离区软删除与还原、进程占用检测 |
| **M3 ✅** | 资产备份（zip + manifest + sha256）、完整性校验、跨机还原 |
| M3.5 | 项目归档迁移（整目录打包 → 校验 → 异地解压） |
| M4 | 桌面壳（Tauri 2 + Vue3）、安装包与签名、规则库云更新 |

## 开发

```bash
cargo test            # 30 项单元测试
cargo clippy --all-targets
cargo fmt
```

需要 Rust stable（见 `rust-toolchain.toml`）。Windows 上需要 MSVC 编译工具链。
