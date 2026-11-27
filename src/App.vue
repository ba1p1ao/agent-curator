<script setup lang="ts">
import { ref } from "vue";
import Overview from "./views/Overview.vue";
import Clean from "./views/Clean.vue";
import Backup from "./views/Backup.vue";
import Vault from "./views/Vault.vue";

type Tab = "overview" | "clean" | "backup" | "vault";

const tabs: { key: Tab; label: string }[] = [
  { key: "overview", label: "体检总览" },
  { key: "clean", label: "清理" },
  { key: "backup", label: "资产备份" },
  { key: "vault", label: "隔离区" },
];

const current = ref<Tab>("overview");
</script>

<template>
  <div class="app">
    <aside class="sidebar">
      <div class="brand">
        <div class="brand-name">AgentCurator</div>
        <div class="brand-sub">司库 · AI agent 数据管家</div>
      </div>
      <button
        v-for="tab in tabs"
        :key="tab.key"
        class="nav-item"
        :class="{ active: current === tab.key }"
        @click="current = tab.key"
      >
        <span class="nav-dot" />
        {{ tab.label }}
      </button>
      <div class="sidebar-foot">
        清理走隔离区，随时可还原<br />
        备份逐条校验 sha256
      </div>
    </aside>

    <main class="main">
      <Overview v-if="current === 'overview'" />
      <Clean v-else-if="current === 'clean'" />
      <Backup v-else-if="current === 'backup'" />
      <Vault v-else />
    </main>
  </div>
</template>
