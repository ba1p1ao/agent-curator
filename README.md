# AgentCurator · 司库

> 给 AI agent 留下的文件做「清产核资」：分清哪些是要长期保管的资产、哪些是可以放心丢掉的痕迹。

`curator` 的本义是**典藏管理者**——给藏品编目、分类，决定哪些入库、哪些出库。
这个工具做的正是这件事，而且**保管和清理同等重要**，不做只删不管的清理器。

> **当前阶段：M1（只读）**
> 只扫描、只出报告，**不删除也不移动任何文件**。
> 清理与备份会在 M2 引入，且必须走「生成计划 → 人工确认 → 隔离区」流程。

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
./target/release/curator rules validate      # 校验规则库
./target/release/curator scan                # 只读体检
./target/release/curator scan --md reports/scan.md --json reports/scan.json
./target/release/curator scan --dump-unmatched reports/unmatched.tsv
```

`--dump-unmatched` 导出未识别清单，是迭代规则库的主要工具：
先看未识别集中在哪些目录，再补规则，把覆盖率从 87% 推到 99%。

输出示例（本机实测，6.0 万文件 / 3.1 GB / 2.2 秒）：

```
AgentCurator v0.1.0  ·  只读体检模式（不会删除、移动任何文件）

按数据性质分类（核心结论）
┌────────────┬────────┬──────────┬───────┬──────────────────────────┐
│ 类别       ┆ 文件数 ┆ 体积     ┆ 占比  ┆ 判定说明                 │
╞════════════╪════════╪══════════╪═══════╪══════════════════════════╡
│ 用户资产   ┆ 32,262 ┆ 1.6 GB   ┆ 38.3% ┆ 不可恢复，禁止清理       │
│ 用户数据   ┆ 14,070 ┆ 1.8 GB   ┆ 42.4% ┆ 用户产出内容，需逐项确认 │
│ 项目产物   ┆  1,329 ┆ 25.4 MB  ┆  0.6% ┆ 可由源码重新构建         │
│ 可重建垃圾 ┆ 20,732 ┆ 755.0 MB ┆ 17.5% ┆ 删除仅丢失历史，可重建   │
│ 未识别     ┆    210 ┆ 50.8 MB  ┆  1.2% ┆ 无规则命中，默认保留     │
└────────────┴────────┴──────────┴───────┴──────────────────────────┘

释放潜力
  可安全释放（低风险可重建）      641.6 MB  (14.9%)
  需人工确认（高风险 / 项目产物）  1.9 GB  (45.6%)
  受保护（资产 + 未识别）          1.7 GB  (39.5%)
```

## 工程结构

```
crates/
  curator-core/          核心库（无 CLI 依赖，后续可被 Tauri 直接引用）
    model.rs             领域词汇：DataClass / Risk / Action / Node
    rules/               规则编译与「最保守优先」裁决
    scanner.rs           只读并行扫描（jwalk）
    classifier.rs        路径 -> Node
    aggregate.rs         流式聚合，不保留全量节点
    report.rs            终端 / Markdown / JSON 三种报表
  curator-cli/           命令 `curator`
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

- 扫描全程只读元数据（size / mtime），不读文件内容，不做 hash；
- 不跟随符号链接与目录联接，避免体积重复统计和越界枚举；
- 无规则命中的路径一律按"保留"处理；
- 带 `protect` 的规则（skills / memory / 配置 / 运行时）任何阶段都不得进入清理清单。

## 路线图

| 阶段 | 内容 |
|---|---|
| **M1（当前）** | 只读扫描 + 规则库 + 体积归类 + 报告导出 |
| M2 | 清理计划、dry-run 清单、隔离区软删除与还原、进程占用检测 |
| M3 | 资产备份（zip + manifest.sha256）、项目归档迁移 |
| M4 | 桌面壳（Tauri 2 + Vue3）、安装包与签名、规则库云更新 |

## 开发

```bash
cargo test            # 单元测试
cargo clippy --all-targets
cargo fmt
```

需要 Rust stable（见 `rust-toolchain.toml`）。Windows 上需要 MSVC 编译工具链。
