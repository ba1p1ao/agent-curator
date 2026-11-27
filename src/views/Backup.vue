<script setup lang="ts">
import ErrorPanel from "../components/ErrorPanel.vue";
import { computed, onMounted, ref } from "vue";
import {
  api,
  errorText,
  humanBytes,
  shortPath,
  thousands,
  type AgentListItem,
  type BackupInfo,
  type BackupResult,
  type VerifyResult,
} from "../api";

const label = ref("");
const agents = ref<AgentListItem[]>([]);
const selectedAgents = ref<string[]>([]);
const nothingSelected = computed(() => selectedAgents.value.length === 0);
const running = ref(false);
const verifying = ref<string | null>(null);
const err = ref("");
const result = ref<BackupResult | null>(null);
const verifyResult = ref<{ name: string; data: VerifyResult } | null>(null);
const backups = ref<BackupInfo[]>([]);
const loadingList = ref(true);

async function loadAgents() {
  try {
    const list = await api.listAgents();
    agents.value = list.filter((a) => a.installed);
    selectedAgents.value = agents.value.map((a) => a.id);
  } catch (e) {
    err.value = errorText(e);
  }
}

async function loadList() {
  loadingList.value = true;
  try {
    backups.value = await api.listBackups();
  } catch (e) {
    err.value = errorText(e);
  } finally {
    loadingList.value = false;
  }
}

async function create() {
  running.value = true;
  err.value = "";
  verifyResult.value = null;
  try {
    result.value = await api.createBackup(
      label.value.trim() || null,
      selectedAgents.value
    );
    await loadList();
  } catch (e) {
    err.value = errorText(e);
  } finally {
    running.value = false;
  }
}

async function verify(item: BackupInfo) {
  verifying.value = item.path;
  err.value = "";
  try {
    const data = await api.verifyBackup(item.path);
    verifyResult.value = { name: item.name, data };
  } catch (e) {
    err.value = errorText(e);
  } finally {
    verifying.value = null;
  }
}

onMounted(() => {
  loadAgents();
  loadList();
});
</script>

<template>
  <div>
    <div class="page-head">
      <h2 class="page-title">资产备份</h2>
      <p class="page-desc">
        只打包删了不可再生的东西：skills、agents、memory、CLAUDE.md、配置。缓存和会话记录不在备份范围内。
      </p>
    </div>

    <ErrorPanel v-if="err" :error="err" :retry="create" />

    <div class="card">
      <h3 class="card-title">创建备份</h3>
      <div class="row">
        <span class="field">
          标签
          <input v-model="label" class="input" placeholder="before-upgrade（可空）" />
        </span>
        <button
          class="btn primary"
          :disabled="running || nothingSelected"
          @click="create"
        >
          <span v-if="running" class="spinner" />
          {{ running ? " 打包中…" : "开始备份" }}
        </button>
      </div>
      <div class="chips">
        <span class="chips-label">备份范围</span>
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
        <button
          v-if="agents.length"
          class="chip ghost"
          @click="selectedAgents = agents.map((a) => a.id)"
        >
          全选
        </button>
      </div>
      <p class="faint" style="margin: 10px 0 0">
        输出到 ~/curator-backups，源文件只读，不会被改动。
      </p>
    </div>

    <div v-if="result" class="card">
      <h3 class="card-title">备份完成</h3>
      <div class="result">
        条目 {{ thousands(result.entries) }} 项
        原始 {{ humanBytes(result.sourceBytes) }} → 压缩后 {{ humanBytes(result.zipBytes) }}
        包   {{ result.path }}
        SHA  {{ result.sha256 }}
        <template v-if="result.failures.length">
          {{ result.failures.length }} 项读取失败，未写入备份
        </template>
      </div>
    </div>

    <div v-if="verifyResult" class="card">
      <h3 class="card-title">校验结果 · {{ verifyResult.name }}</h3>
      <div
        class="banner"
        :class="
          verifyResult.data.mismatched.length || verifyResult.data.missing.length ? 'danger' : 'info'
        "
      >
        {{ verifyResult.data.ok }} 通过 /
        {{ verifyResult.data.mismatched.length }} 不匹配 /
        {{ verifyResult.data.missing.length }} 缺失
        <template v-if="!verifyResult.data.mismatched.length && !verifyResult.data.missing.length">
          —— 备份包完整
        </template>
      </div>
      <div v-for="m in verifyResult.data.mismatched" :key="m[0]" class="mono">
        {{ m[0] }} : {{ m[1] }}
      </div>
    </div>

    <div class="card">
      <h3 class="card-title">已有备份</h3>
      <div v-if="loadingList" class="empty">读取中…</div>
      <div v-else-if="!backups.length" class="empty">还没有备份。建议现在做一个。</div>
      <table v-else class="table">
        <thead>
          <tr>
            <th>文件</th>
            <th>创建时间</th>
            <th class="num">条目</th>
            <th class="num">大小</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="item in backups" :key="item.path">
            <td class="path" :title="item.path">{{ shortPath(item.name, 44) }}</td>
            <td class="muted">{{ item.createdAt ?? "-" }}</td>
            <td class="num">{{ item.entries === null ? "-" : thousands(item.entries) }}</td>
            <td class="num">{{ humanBytes(item.sizeBytes) }}</td>
            <td>
              <button
                class="btn small"
                :disabled="verifying === item.path"
                @click="verify(item)"
              >
                {{ verifying === item.path ? "校验中…" : "校验" }}
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
