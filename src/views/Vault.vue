<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import {
  api,
  errorText,
  humanBytes,
  shortPath,
  thousands,
  type RestoreResult,
  type RunInfo,
} from "../api";

const runs = ref<RunInfo[]>([]);
const usage = ref<{ runs: number; bytes: number; path: string } | null>(null);
const loading = ref(true);
const err = ref("");
const target = ref<string | null>(null);
const confirmed = ref(false);
const working = ref(false);
const result = ref<RestoreResult | null>(null);

const selected = computed(() => runs.value.find((r) => r.runId === target.value) ?? null);

async function load() {
  loading.value = true;
  err.value = "";
  try {
    runs.value = await api.listRuns();
    usage.value = await api.binUsage();
  } catch (e) {
    err.value = errorText(e);
  } finally {
    loading.value = false;
  }
}

function pick(run: RunInfo) {
  target.value = run.runId;
  confirmed.value = false;
  result.value = null;
}

async function restore() {
  if (!target.value) return;
  working.value = true;
  err.value = "";
  try {
    result.value = await api.restore(target.value);
    target.value = null;
    confirmed.value = false;
    await load();
  } catch (e) {
    err.value = errorText(e);
  } finally {
    working.value = false;
  }
}

onMounted(load);
</script>

<template>
  <div>
    <div class="page-head">
      <h2 class="page-title">隔离区</h2>
      <p class="page-desc">
        清理掉的每个文件都躺在这里，按批次记录原路径和校验信息。还原不会覆盖已存在的文件。
      </p>
    </div>

    <div v-if="err" class="banner danger">{{ err }}</div>

    <div v-if="usage" class="banner info">
      隔离区 {{ usage.path }} · {{ usage.runs }} 个批次 · 占用 {{ humanBytes(usage.bytes) }}
    </div>

    <div class="card">
      <h3 class="card-title">批次</h3>
      <div v-if="loading" class="empty">读取中…</div>
      <div v-else-if="!runs.length" class="empty">还没有任何批次。去「清理」页面执行一次试试。</div>
      <table v-else class="table">
        <thead>
          <tr>
            <th></th>
            <th>批次号</th>
            <th>时间</th>
            <th>状态</th>
            <th class="num">文件数</th>
            <th class="num">体积</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="run in runs" :key="run.runId" :class="{ selected: target === run.runId }">
            <td style="width: 28px">
              <input
                type="radio"
                name="run"
                :checked="target === run.runId"
                :disabled="run.status === 'restored'"
                @change="pick(run)"
              />
            </td>
            <td class="mono">{{ run.runId }}</td>
            <td class="muted">{{ run.createdAt }}</td>
            <td>
              <span class="tag" :class="run.status === 'restored' ? 'unknown' : 'ephemeral'">
                {{ run.status === "restored" ? "已还原" : run.status === "partial" ? "部分成功" : "已隔离" }}
              </span>
            </td>
            <td class="num">{{ thousands(run.items) }}</td>
            <td class="num">{{ humanBytes(run.bytes) }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <div v-if="selected" class="card">
      <h3 class="card-title">还原 {{ selected.runId }}</h3>
      <p class="muted" style="margin: 0 0 12px">
        将把 {{ thousands(selected.items) }} 项 / {{ humanBytes(selected.bytes) }} 移回原始路径。
        原路径已有同名文件时会跳过并列出，不会覆盖。
      </p>
      <label class="check">
        <input v-model="confirmed" type="checkbox" />
        我确认要还原这个批次
      </label>
      <div class="row" style="margin-top: 10px">
        <button class="btn primary" :disabled="!confirmed || working" @click="restore">
          <span v-if="working" class="spinner" />
          {{ working ? " 还原中…" : "执行还原" }}
        </button>
        <button class="btn" :disabled="working" @click="target = null">取消</button>
      </div>
    </div>

    <div v-if="result" class="card">
      <h3 class="card-title">还原结果</h3>
      <div class="result">
        已还原 {{ thousands(result.restored) }} 项 / {{ humanBytes(result.bytes) }}
        <template v-if="result.conflicts.length">
          冲突 {{ result.conflicts.length }} 项（未覆盖，仍在隔离区）
        </template>
        <template v-if="result.failures.length">
          失败 {{ result.failures.length }} 项
        </template>
      </div>
      <div v-if="result.conflicts.length" style="margin-top: 10px">
        <div v-for="c in result.conflicts.slice(0, 10)" :key="c[0]" class="mono">
          {{ shortPath(c[0], 60) }} — {{ c[1] }}
        </div>
      </div>
    </div>
  </div>
</template>
