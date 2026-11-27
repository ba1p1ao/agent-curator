export type Friendly = {
  title: string;
  steps: string[];
  raw: string;
};

/**
 * 把底层错误翻译成"发生了什么 + 该怎么办"。
 *
 * 原则：界面上永远不出现"设置环境变量 XXX"这类开发者指令。
 * 用户不关心实现，只关心下一步点哪里。
 */
export function friendlyError(e: unknown): Friendly {
  const raw = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
  const lower = raw.toLowerCase();

  if (lower.includes("rules") || raw.includes("规则库")) {
    return {
      title: "程序没能加载规则库，暂时无法判断哪些文件可以动。",
      steps: [
        "使用最新版本的安装包：规则已经内置在程序里，正常情况下不需要任何配置。",
        "如果你手动改过 rules 文件夹，把它整个删除再重启，即可恢复默认规则。",
        "仍然失败的话，把下方详细信息发给开发者。",
      ],
      raw,
    };
  }

  if (
    lower.includes("permission") ||
    lower.includes("access is denied") ||
    raw.includes("拒绝访问") ||
    raw.includes("权限")
  ) {
    return {
      title: "读取某些目录时被系统拒绝了。",
      steps: [
        "关闭程序，右键以管理员身份重新打开。",
        "检查安全软件是否拦截了对用户目录（如 .claude / .workbuddy）的读取。",
        "如果只想看了部分目录，可以先用命令行指定范围。",
      ],
      raw,
    };
  }

  if (raw.includes("未发现任何") || raw.includes("没有可备份")) {
    return {
      title: "没有找到可以体检的 AI agent 目录。",
      steps: [
        "先安装 Claude Code、Codex 或 WorkBuddy，并至少完整运行一次，让它们生成配置目录。",
        "确认这些目录在你当前用户的主目录下（形如 C:/Users/你/.claude）。",
        "确认后点重试。",
      ],
      raw,
    };
  }

  if (raw.includes("计划为空")) {
    return {
      title: "没有需要清理的内容。",
      steps: [
        "这通常是好消息：缓存和日志已经被清过了。",
        "如果确实想清理更多，可以在筛选里勾选「需人工确认」项。",
      ],
      raw,
    };
  }

  return {
    title: "遇到了一个未预期的问题。",
    steps: ["先点「重试」。", "如果反复出现，把下方详细信息发给开发者。"],
    raw,
  };
}
