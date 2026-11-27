//! AgentCurator 核心库
//!
//! M1 阶段只做三件事：**发现 → 归类 → 出报告**，全程只读，
//! 不包含任何删除、移动或写入被扫描目录的代码路径。

pub mod aggregate;
pub mod classifier;
pub mod model;
pub mod plan;
pub mod procs;
pub mod quarantine;
pub mod report;
pub mod rules;
pub mod scanner;
pub mod util;

pub use model::{Action, DataClass, Node, Risk};
pub use rules::RuleLibrary;
