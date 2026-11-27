<script setup lang="ts">
import { computed, ref } from "vue";
import Overview from "./views/Overview.vue";
import Clean from "./views/Clean.vue";
import Backup from "./views/Backup.vue";
import Vault from "./views/Vault.vue";

type Tab = "overview" | "clean" | "backup" | "vault";

const tabs: { key: Tab; label: string; hint: string }[] = [
  { key: "overview", label: "体检总览", hint: "看清家底" },
  { key: "clean", label: "清理", hint: "先进隔离区" },
  { key: "backup", label: "资产备份", hint: "skills / 记忆" },
  { key: "vault", label: "隔离区", hint: "可还原" },
];

const current = ref<Tab>("overview");

const view = computed(() => {
  switch (current.value) {
    case "clean":
      return Clean;
    case "backup":
      return Backup;
    case "vault":
      return Vault;
    default:
      return Overview;
  }
});

function go(tab: string) {
  current.value = tab as Tab;
}
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
        @click="go(tab.key)"
      >
        <span class="nav-dot" />
        <span class="nav-label">{{ tab.label }}</span>
        <span class="nav-hint">{{ tab.hint }}</span>
      </button>
      <div class="sidebar-foot">
        清理走隔离区，随时可还原<br />
        备份逐条校验 sha256
      </div>
    </aside>

    <main class="main">
      <!-- 总览的扫描要 2-10 秒，切页时不重复触发 -->
      <KeepAlive :include="['Overview']">
        <component :is="view" @navigate="go" />
      </KeepAlive>
    </main>
  </div>
</template>

<style scoped>
.nav-label {
  flex: 1;
}

.nav-hint {
  font-size: 10.5px;
  color: var(--faint);
  white-space: nowrap;
}

.nav-item.active .nav-hint {
  color: var(--accent);
  opacity: 0.7;
}
</style>
