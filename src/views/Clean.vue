<script setup lang="ts">
import { computed, ref } from "vue";
import {
  api,
  classLabel,
  errorText,
  humanBytes,
  riskLabel,
  shortPath,
  thousands,
  type ApplyResult,
  type CleanOptions,
  type Plan,
} from "../api";

const includeReview = ref(false);
const includeRunning = ref(false);
const minSizeMb = ref(0);
const agentFilter = ref("");
const confirmed = ref(false);

const planning = ref(false);
const applying = ref(false);
const err = ref("");
const plan = ref<Plan | null>(null);
const result = ref<ApplyResult | null>(null);

const options = computed<CleanOptions>(() => ({
  includeReview: includeReview.value,
  includeRunning: includeRunning.value,
  minSize: Math.max(0, Math.round(minSizeMb.value * 1024 * 1024)),
  agents: agentFilter.value
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean),
}));

function sum(bytes: number[]): number {
  return bytes.reduce((a, b) => a + b, 0);
}

const planBytes = computed(() =>
  plan.value ? sum(plan.value.items.map((i) => i.bytes)) : 0
);

const skippedBytes = computed(() =>
  plan.value ? sum(plan.value.skipped.map((i) => i.bytes)) : 0
);

const visibleItems = computed(() => plan.value?.items.slice(0, 80) ?? []);

async function makePlan() {
  planning.value = true;
  err.value = "";
  result.value = null;
  confirmed.value = false;
  try {
    plan.value = await api.plan(options.value);
  } catch (e) {
    err.value = errorText(e);
  } finally {
    planning.value = false;
  }
}

async function applyPlan() {
  if (!plan.value || !plan.value.items.length) return;
  applying.value = true;
  err.value = "";
  try {
    result.value = await api.apply(options.value);
    plan.value = null;
    confirmed.value = false;
  } catch (e) {
    err.value = errorText(e);
  } finally {
    applying.value = false;
  }
}
</script>

<template>
  <div>
    <div class="page-head">
      <h2 class="page-title">清理</h2>
      <p class="page-desc">
        先生成计划看清楚要动哪些文件，确认后才移入隔离区。隔离区可随时还原，不会直接删除。
      </p>
    </div>

    <div v-if="err" class="banner danger">{{ err }}</div>

    <div class="card">
      <h3 class="card-title">筛选条件</h3>
      <div class="row">
        <label class="check">
          <input v-model="includeReview" type="checkbox" />
          连带清理「需人工确认」项
        </label>
        <label class="check">
          <input v-model="includeRunning" type="checkbox" />
          允许清理正在运行的 agent
        </label>
        <span class="field">
          忽略小于
          <input v-model.number="minSizeMb" class="input" style="min-width: 70px" type="number" min="0" />
          MB 的单项
        </span>
        <span class="field">
          仅限 agent
          <input v-model="agentFilter" class="input" placeholder="codex, workbuddy" />
        </span>
      </div>
      <div v-if="includeRunning" class="banner danger" style="margin-top: 12px">
        允许清理运行中的 agent 有损坏会话与索引库的风险。确认相关 agent 已退出后再勾选。
      </div>
    </div>

    <div class="row" style="margin-bottom: 14px">
      <button class="btn primary" :disabled="planning || applying" @click="makePlan">
        <span v-if="planning" class="spinner" />
        {{ planning ? " 生成中…" : "生成清理计划" }}
      </button>
      <span v-if="plan" class="muted">
        将移动 {{ thousands(plan.items.length) }} 项 / {{ humanBytes(planBytes) }}
      </span>
    </div>

    <div v-if="plan" class="card">
      <h3 class="card-title">
        计划明细
        <span class="muted" style="font-weight: 400">
          （展示前 {{ visibleItems.length }} 项，共 {{ thousands(plan.items.length) }} 项）
        </span>
      </h3>

      <div v-if="!plan.items.length" class="empty">没有可清理的条目。</div>

      <template v-else>
        <table class="table">
          <thead>
            <tr>
              <th style="width: 42px" class="num">#</th>
              <th>路径</th>
              <th class="num">体积</th>
              <th>类别</th>
              <th>风险</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in visibleItems" :key="item.index">
              <td class="num faint">{{ item.index }}</td>
              <td class="path">{{ shortPath(item.path) }}</td>
              <td class="num">{{ humanBytes(item.bytes) }}</td>
              <td><span class="tag" :class="item.class">{{ classLabel(item.class) }}</span></td>
              <td class="muted">{{ riskLabel(item.risk) }}</td>
            </tr>
          </tbody>
        </table>
      </template>

      <div v-if="plan.skipped.length" style="margin-top: 14px">
        <h3 class="card-title">已跳过 {{ thousands(plan.skipped.length) }} 项 / {{ humanBytes(skippedBytes) }}</h3>
        <table class="table">
          <thead>
            <tr>
              <th>路径</th>
              <th>原因</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(item, i) in plan.skipped.slice(0, 10)" :key="i">
              <td class="path">{{ shortPath(item.path, 60) }}</td>
              <td class="muted">{{ item.reason }}</td>
            </tr>
          </tbody>
        </table>
      </div>

      <div style="margin-top: 16px; border-top: 1px solid var(--border); padding-top: 14px">
        <label class="check">
          <input v-model="confirmed" type="checkbox" />
          我确认要把以上 {{ thousands(plan.items.length) }} 项移入隔离区
        </label>
        <div class="row" style="margin-top: 10px">
          <button class="btn danger" :disabled="!confirmed || applying" @click="applyPlan">
            <span v-if="applying" class="spinner" />
            {{ applying ? " 执行中…" : "执行清理" }}
          </button>
          <button class="btn" :disabled="applying" @click="plan = null">取消</button>
        </div>
      </div>
    </div>

    <div v-if="result" class="card">
      <h3 class="card-title">已移入隔离区</h3>
      <div class="result">
        批次号 {{ result.runId }}
        移动 {{ thousands(result.moved) }} 项 / {{ humanBytes(result.bytes) }}
        位置 {{ result.runDir }}
        <template v-if="result.failures.length">
          {{ result.failures.length }} 项失败
        </template>
      </div>
      <p class="muted" style="margin: 10px 0 0">
        去「隔离区」页面可以随时还原。
      </p>
    </div>
  </div>
</template>
