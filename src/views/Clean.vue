<script setup lang="ts">
import ErrorPanel from "../components/ErrorPanel.vue";
import { computed, onMounted, ref, watch } from "vue";
import {
  api,
  classLabel,
  errorText,
  humanBytes,
  riskLabel,
  shortPath,
  thousands,
  type AgentListItem,
  type ApplyResult,
  type CleanOptions,
  type Plan,
  type PlanItem,
} from "../api";

const emit = defineEmits<{ (e: "navigate", tab: string): void }>();

const includeReview = ref(true);
const includeRunning = ref(false);
const minSizeMb = ref(0);

const agents = ref<AgentListItem[]>([]);
const uncovered = ref<string[]>([]);
const selectedAgents = ref<string[]>([]);

const planning = ref(false);
const applying = ref(false);
const err = ref("");
const plan = ref<Plan | null>(null);
const planId = ref("");
const selected = ref<Set<number>>(new Set());
const expanded = ref<string | null>(null);
const confirmed = ref(false);
const result = ref<ApplyResult | null>(null);

const nothingSelected = computed(() => selectedAgents.value.length === 0);

const options = computed<CleanOptions>(() => ({
  includeReview: includeReview.value,
  includeRunning: includeRunning.value,
  minSize: Math.max(0, Math.round(minSizeMb.value * 1024 * 1024)),
  agents: selectedAgents.value,
}));

type Group = {
  key: string;
  cls: string;
  risk: string;
  items: PlanItem[];
  bytes: number;
};

/// 按规则分组。几千条明细平铺既卡又难选，分组后默认只渲染分组行。
const groups = computed<Group[]>(() => {
  if (!plan.value) return [];
  const map = new Map<string, Group>();
  for (const item of plan.value.items) {
    const key = item.rule_id ?? "(无规则)";
    let group = map.get(key);
    if (!group) {
      group = { key, cls: item.class, risk: item.risk, items: [], bytes: 0 };
      map.set(key, group);
    }
    group.items.push(item);
    group.bytes += item.bytes;
  }
  return [...map.values()].sort((a, b) => b.bytes - a.bytes);
});

const planBytes = computed(() =>
  plan.value ? plan.value.items.reduce((a, b) => a + b.bytes, 0) : 0
);

const selectedCount = computed(() => selected.value.size);

const selectedBytes = computed(() => {
  if (!plan.value) return 0;
  let sum = 0;
  for (const item of plan.value.items) {
    if (selected.value.has(item.index)) sum += item.bytes;
  }
  return sum;
});

function groupState(group: Group): "all" | "none" | "part" {
  let hit = 0;
  for (const item of group.items) {
    if (selected.value.has(item.index)) hit++;
  }
  if (hit === 0) return "none";
  return hit === group.items.length ? "all" : "part";
}

function toggleGroup(group: Group) {
  const next = new Set(selected.value);
  if (groupState(group) === "all") {
    for (const item of group.items) next.delete(item.index);
  } else {
    for (const item of group.items) next.add(item.index);
  }
  selected.value = next;
}

function toggleItem(index: number) {
  const next = new Set(selected.value);
  if (next.has(index)) next.delete(index);
  else next.add(index);
  selected.value = next;
}

function selectAll() {
  selected.value = new Set(plan.value?.items.map((i) => i.index) ?? []);
}

function selectNone() {
  selected.value = new Set();
}

function invertSelection() {
  const next = new Set<number>();
  for (const item of plan.value?.items ?? []) {
    if (!selected.value.has(item.index)) next.add(item.index);
  }
  selected.value = next;
}

function selectLowRisk() {
  const next = new Set<number>();
  for (const item of plan.value?.items ?? []) {
    if (item.risk === "low") next.add(item.index);
  }
  selected.value = next;
}

async function loadAgents() {
  try {
    const list = await api.listAgents();
    agents.value = list.filter((a) => a.installed && a.hasRules);
    uncovered.value = list.filter((a) => a.installed && !a.hasRules).map((a) => a.name);
    selectedAgents.value = agents.value.map((a) => a.id);
  } catch (e) {
    err.value = errorText(e);
  }
}

onMounted(loadAgents);

// 筛选条件一变，旧计划连同勾选一起作废：下标只对同一份计划有效
watch(
  [includeReview, includeRunning, minSizeMb, selectedAgents],
  () => {
    if (plan.value) {
      plan.value = null;
      planId.value = "";
      selected.value = new Set();
      confirmed.value = false;
    }
  },
  { deep: true }
);

async function makePlan() {
  planning.value = true;
  err.value = "";
  result.value = null;
  confirmed.value = false;
  try {
    const resp = await api.plan(options.value);
    plan.value = resp.plan;
    planId.value = resp.planId;
    // 默认只勾「可直接清」的条目，需人工确认的留给用户自己决定
    selected.value = new Set(
      resp.plan.items.filter((i) => i.action !== "review").map((i) => i.index)
    );
  } catch (e) {
    err.value = errorText(e);
  } finally {
    planning.value = false;
  }
}

async function applyPlan() {
  if (!plan.value || selectedCount.value === 0) return;
  applying.value = true;
  err.value = "";
  try {
    result.value = await api.apply(planId.value, [...selected.value]);
    plan.value = null;
    planId.value = "";
    selected.value = new Set();
    confirmed.value = false;
  } catch (e) {
    err.value = errorText(e);
  } finally {
    applying.value = false;
  }
}

function discardPlan() {
  plan.value = null;
  planId.value = "";
  selected.value = new Set();
  confirmed.value = false;
}
</script>

<template>
  <div>
    <div class="page-head">
      <h2 class="page-title">清理</h2>
      <p class="page-desc">
        先生成计划，再逐条挑选。执行时只动你勾选的部分，全部移入隔离区，可随时还原。
      </p>
    </div>

    <ErrorPanel v-if="err" :error="err" :retry="makePlan" />

    <div class="card">
      <h3 class="card-title">筛选</h3>
      <div class="row">
        <label class="check">
          <input v-model="includeReview" type="checkbox" />
          连「需人工确认」项一起列出
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
      </div>

      <div class="chips">
        <span class="chips-label">处理范围</span>
        <label
          v-for="agent in agents"
          :key="agent.id"
          class="chip"
          :class="{ on: selectedAgents.includes(agent.id) }"
        >
          <input v-model="selectedAgents" type="checkbox" :value="agent.id" />
          {{ agent.name }}
        </label>
        <span v-if="!agents.length" class="faint">未检测到已安装的 agent</span>
        <span v-if="uncovered.length" class="faint">
          · {{ uncovered.join("、") }} 已安装但暂无规则，本次不处理
        </span>
        <button
          v-if="agents.length"
          class="chip ghost"
          @click="selectedAgents = agents.map((a) => a.id)"
        >
          全选
        </button>
      </div>
    </div>

    <div class="row" style="margin-bottom: 14px">
      <button
        class="btn primary"
        :disabled="planning || applying || nothingSelected"
        @click="makePlan"
      >
        <span v-if="planning" class="spinner" />
        {{ planning ? " 生成中…" : plan ? "重新生成计划" : "生成清理计划" }}
      </button>
      <span v-if="nothingSelected" class="faint">请至少选择一个 agent</span>
      <span v-if="plan" class="muted">
        共 {{ thousands(plan.items.length) }} 项 / {{ humanBytes(planBytes) }}
      </span>
    </div>

    <div v-if="plan" class="card">
      <div class="row" style="justify-content: space-between; margin-bottom: 10px">
        <h3 class="card-title" style="margin: 0">计划明细</h3>
        <div class="row" style="gap: 6px">
          <button class="btn small" @click="selectAll">全选</button>
          <button class="btn small" @click="selectNone">全不选</button>
          <button class="btn small" @click="invertSelection">反选</button>
          <button class="btn small" @click="selectLowRisk">只选低风险</button>
        </div>
      </div>

      <div v-if="!plan.items.length" class="empty">没有可清理的条目。</div>

      <table v-else class="table">
        <thead>
          <tr>
            <th style="width: 34px"></th>
            <th>规则</th>
            <th>类别</th>
            <th>风险</th>
            <th class="num">项数</th>
            <th class="num">体积</th>
          </tr>
        </thead>
        <tbody>
          <template v-for="group in groups" :key="group.key">
            <tr
              class="group-row"
              @click="expanded = expanded === group.key ? null : group.key"
            >
              <td>
                <input
                  type="checkbox"
                  :checked="groupState(group) === 'all'"
                  :indeterminate.prop="groupState(group) === 'part'"
                  @click.stop
                  @change="toggleGroup(group)"
                />
              </td>
              <td class="mono">
                {{ expanded === group.key ? "▾" : "▸" }} {{ group.key }}
              </td>
              <td>
                <span class="tag" :class="group.cls">{{ classLabel(group.cls) }}</span>
              </td>
              <td class="muted">{{ riskLabel(group.risk) }}</td>
              <td class="num">{{ thousands(group.items.length) }}</td>
              <td class="num">{{ humanBytes(group.bytes) }}</td>
            </tr>

            <tr v-if="expanded === group.key">
              <td></td>
              <td colspan="5" style="padding: 0">
                <div class="items">
                  <div class="faint" style="padding: 6px 8px">
                    显示前 {{ Math.min(group.items.length, 100) }} 项，共
                    {{ thousands(group.items.length) }} 项
                  </div>
                  <label
                    v-for="item in group.items.slice(0, 100)"
                    :key="item.index"
                    class="item-row"
                  >
                    <input
                      type="checkbox"
                      :checked="selected.has(item.index)"
                      @change="toggleItem(item.index)"
                    />
                    <span class="path">{{ shortPath(item.path, 96) }}</span>
                    <span class="num muted">{{ humanBytes(item.bytes) }}</span>
                  </label>
                </div>
              </td>
            </tr>
          </template>
        </tbody>
      </table>

      <div v-if="plan.skipped.length" style="margin-top: 14px">
        <h3 class="card-title">
          已跳过 {{ thousands(plan.skipped.length) }} 项 /
          {{ humanBytes(plan.skipped.reduce((a, b) => a + b.bytes, 0)) }}
        </h3>
        <table class="table">
          <thead>
            <tr>
              <th>路径</th>
              <th>原因</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(item, i) in plan.skipped.slice(0, 8)" :key="i">
              <td class="path">{{ shortPath(item.path, 60) }}</td>
              <td class="muted">{{ item.reason }}</td>
            </tr>
          </tbody>
        </table>
      </div>

      <div class="footer">
        <div class="muted" style="margin-bottom: 8px">
          已勾选 <strong>{{ thousands(selectedCount) }}</strong> 项 /
          {{ humanBytes(selectedBytes) }}
        </div>
        <label class="check">
          <input v-model="confirmed" type="checkbox" />
          确认把勾选的 {{ thousands(selectedCount) }} 项移入隔离区
        </label>
        <div class="row" style="margin-top: 10px">
          <button
            class="btn danger"
            :disabled="!confirmed || applying || selectedCount === 0"
            @click="applyPlan"
          >
            <span v-if="applying" class="spinner" />
            {{ applying ? " 执行中…" : "执行清理" }}
          </button>
          <button class="btn" :disabled="applying" @click="discardPlan">取消</button>
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
      <div class="row" style="margin-top: 12px">
        <button class="btn primary" @click="emit('navigate', 'vault')">
          去隔离区看看
        </button>
        <button class="btn" @click="result = null">继续清理</button>
        <span class="faint">还原不会覆盖已有文件</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.group-row {
  cursor: pointer;
}

.group-row:hover {
  background: #fafaf7;
}

.items {
  background: #fcfcfa;
  border-top: 1px solid var(--border);
  border-bottom: 1px solid var(--border);
  max-height: 340px;
  overflow-y: auto;
}

.item-row {
  display: grid;
  grid-template-columns: 24px 1fr auto;
  gap: 10px;
  align-items: center;
  padding: 3px 10px 3px 18px;
  cursor: pointer;
}

.item-row:hover {
  background: #f4f4ef;
}

.footer {
  margin-top: 16px;
  padding-top: 14px;
  border-top: 1px solid var(--border);
}
</style>
