import { invoke } from "@tauri-apps/api/core";

export interface Bucket {
  key: string;
  label: string;
  files: number;
  bytes: number;
}

export interface RootInfo {
  agent: string;
  path: string;
  files: number;
  bytes: number;
}

export interface ScanResult {
  totalFiles: number;
  totalBytes: number;
  elapsedMs: number;
  safeBytes: number;
  confirmBytes: number;
  protectedBytes: number;
  agents: Bucket[];
  classes: Bucket[];
  roots: RootInfo[];
  ruleCount: number;
  running: string[];
  generatedAt: string;
}

export interface PlanItem {
  index: number;
  path: string;
  bytes: number;
  agent: string;
  class: string;
  risk: string;
  action: string;
  rule_id: string | null;
}

export interface SkippedItem {
  path: string;
  bytes: number;
  rule_id: string | null;
  reason: string;
}

export interface Plan {
  generated_at: string;
  options: Record<string, unknown>;
  running_agents: string[];
  items: PlanItem[];
  skipped: SkippedItem[];
}

export interface ApplyResult {
  runId: string;
  moved: number;
  bytes: number;
  failures: [string, string][];
  runDir: string;
}

export interface RunInfo {
  runId: string;
  createdAt: string;
  status: string;
  items: number;
  bytes: number;
}

export interface RestoreResult {
  restored: number;
  bytes: number;
  conflicts: [string, string][];
  failures: [string, string][];
}

export interface BackupInfo {
  name: string;
  path: string;
  sizeBytes: number;
  createdAt: string | null;
  entries: number | null;
  valid: boolean;
}

export interface BackupResult {
  path: string;
  entries: number;
  sourceBytes: number;
  zipBytes: number;
  sha256: string;
  failures: [string, string][];
}

export interface VerifyResult {
  entries: number;
  ok: number;
  mismatched: [string, string][];
  missing: string[];
}

export interface CleanOptions {
  includeReview: boolean;
  includeRunning: boolean;
  minSize: number;
  agents: string[];
}

export const api = {
  rulesInfo: () =>
    invoke<{ count: number; agents: [string, string][]; roots: number }>("rules_info"),
  scan: () => invoke<ScanResult>("scan_all"),
  plan: (opts: CleanOptions) => invoke<Plan>("plan_clean", { opts }),
  apply: (opts: CleanOptions) => invoke<ApplyResult>("apply_clean", { opts }),
  listRuns: () => invoke<RunInfo[]>("list_runs"),
  restore: (runId: string) => invoke<RestoreResult>("restore_run", { runId }),
  binUsage: () => invoke<{ runs: number; bytes: number; path: string }>("bin_usage"),
  createBackup: (label: string | null, agents: string[]) =>
    invoke<BackupResult>("create_backup", { label, agents }),
  listBackups: () => invoke<BackupInfo[]>("list_backups"),
  verifyBackup: (path: string) => invoke<VerifyResult>("verify_backup", { path }),
};

export function humanBytes(n: number): string {
  if (!n) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(1)} ${units[i]}`;
}

export function thousands(n: number): string {
  return n.toLocaleString("zh-CN");
}

const CLASS_LABEL: Record<string, string> = {
  asset: "用户资产",
  workspace: "用户数据",
  product: "项目产物",
  ephemeral: "可重建垃圾",
  unknown: "未识别",
};

export function classLabel(key: string): string {
  return CLASS_LABEL[key] ?? key;
}

const RISK_LABEL: Record<string, string> = {
  low: "低",
  medium: "中",
  high: "高",
  critical: "极高",
};

export function riskLabel(key: string): string {
  return RISK_LABEL[key] ?? key;
}

export function shortPath(p: string, max = 72): string {
  const s = p.replace(/\\/g, "/");
  return s.length <= max ? s : "…" + s.slice(s.length - max + 1);
}

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}
