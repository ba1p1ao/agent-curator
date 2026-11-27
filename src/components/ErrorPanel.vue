<script setup lang="ts">
import { computed, ref } from "vue";
import { friendlyError } from "../errors";

const props = defineProps<{
  error: unknown;
  retry?: () => void;
}>();

const showDetail = ref(false);
const info = computed(() => friendlyError(props.error));
</script>

<template>
  <div class="errbox">
    <div class="err-title">{{ info.title }}</div>
    <ol class="err-steps">
      <li v-for="(step, i) in info.steps" :key="i">{{ step }}</li>
    </ol>
    <div class="row">
      <button v-if="retry" class="btn primary" @click="retry()">重试</button>
      <button class="btn" @click="showDetail = !showDetail">
        {{ showDetail ? "收起详细信息" : "查看详细信息" }}
      </button>
    </div>
    <pre v-if="showDetail" class="err-detail">{{ info.raw }}</pre>
  </div>
</template>
