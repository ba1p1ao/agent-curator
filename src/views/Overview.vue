<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, classLabel, errorText, humanBytes, thousands, type ScanResult } from "../api";

defineOptions({ name: "Overview" });
const emit = defineEmits<{ (e: "navigate", tab: string): void }>();

const loading = ref(true);
const err = ref("");
const data = ref<ScanResult | null>(null);

async function load() {
  loading.value = true;
  err.value = "";
  try {
    data.value = await api.scan();
  } catch (e) {
    err.value = errorText(e);
  } finally {
    loading.value = false;
  }
}

onMounted(load);
</script>

<template>
  <div>
    <div class="page-head">
      <h2 class="page-title">体检总览</h2>
      <p class="page-desc">
        只读扫描已安装 AI agent 的数据目录，按「能不能删」把文件分成五类。这里不会修改任何文件。
      </p>
    </div>

    <div v-if="err" class="banner danger">{{ err }}</div>

    <div v-if="loading" class="empty">正在扫描…（首次约 2-10 秒）</div>

    <template v-else-if="data">
      <div v-if="data.running.length" class="banner warn">
        检测到正在运行：{{ data.running.join("、") }}。清理时这些 agent 的条目会被自动跳过。
      </div>

      <div class="kpis">
        <div class="kpi">
          <div class="kpi-label">总占用</div>
          <div class="kpi-value">{{ humanBytes(data.totalBytes) }}</div>
          <div class="kpi-note">{{ thousands(data.totalFiles) }} 个文件</div>
        </div>
        <div class="kpi safe">
          <div class="kpi-label">可安全释放</div>
          <div class="kpi-value">{{ humanBytes(data.safeBytes) }}</div>
          <div class="kpi-note">低风险可重建</div>
        </div>
        <div class="kpi confirm">
          <div class="kpi-label">需人工确认</div>
          <div class="kpi-value">{{ humanBytes(data.confirmBytes) }}</div>
          <div class="kpi-note">高风险垃圾 / 用户数据 / 产物</div>
        </div>
        <div class="kpi protect">
          <div class="kpi-label">受保护</div>
          <div class="kpi-value">{{ humanBytes(data.protectedBytes) }}</div>
          <div class="kpi-note">资产 + 未识别，永不清理</div>
        </div>
      </div>

      <div class="row" style="margin-bottom: 14px">
        <button class="btn primary" @click="emit('navigate', 'clean')">
          查看清理计划
        </button>
        <button class="btn" @click="emit('navigate', 'backup')">备份资产</button>
        <button class="btn" @click="emit('navigate', 'vault')">隔离区</button>
        <span class="faint">清理和备份都不会直接删除东西</span>
      </div>

      <div class="card">
        <h3 class="card-title">按数据性质分类</h3>
        <table class="table">
          <thead>
            <tr>
              <th>类别</th>
              <th class="num">文件数</th>
              <th class="num">体积</th>
              <th class="num">占比</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="row in data.classes" :key="row.key">
              <td>
                <span class="tag" :class="row.key">{{ classLabel(row.key) }}</span>
              </td>
              <td class="num">{{ thousands(row.files) }}</td>
              <td class="num">{{ humanBytes(row.bytes) }}</td>
              <td class="num">
                {{ data.totalBytes ? ((row.bytes / data.totalBytes) * 100).toFixed(1) + "%" : "-" }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <div class="card">
        <h3 class="card-title">按 Agent 分布</h3>
        <table class="table">
          <thead>
            <tr>
              <th>Agent</th>
              <th>路径</th>
              <th class="num">文件数</th>
              <th class="num">体积</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="row in data.roots" :key="row.path">
              <td>{{ row.agent }}</td>
              <td class="path">{{ row.path }}</td>
              <td class="num">{{ thousands(row.files) }}</td>
              <td class="num">{{ humanBytes(row.bytes) }}</td>
            </tr>
          </tbody>
        </table>
      </div>

      <div class="row">
        <button class="btn" :disabled="loading" @click="load">重新扫描</button>
        <span class="faint">
          规则 {{ data.ruleCount }} 条（{{ data.ruleSource }}）· 耗时 {{ data.elapsedMs }} ms ·
          {{ data.generatedAt }}
        </span>
      </div>
    </template>
  </div>
</template>
