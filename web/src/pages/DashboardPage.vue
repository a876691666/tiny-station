<script setup lang="ts">
import { computed } from 'vue'
import {
  NCard,
  NGrid,
  NGridItem,
  NStatistic,
  NDescriptions,
  NDescriptionsItem,
  NTag,
  NAlert,
} from 'naive-ui'
import { useStatus } from '../composables'

const { status, error } = useStatus()

const direct = computed(
  () => status.value?.nodes.filter((n) => n.is_self || n.directly_reachable).length ?? 0,
)
</script>

<template>
  <n-alert v-if="error" type="error" :show-icon="false" style="margin-bottom: 16px">
    {{ error }}
  </n-alert>

  <template v-if="status">
    <n-grid :cols="4" :x-gap="16" :y-gap="16">
      <n-grid-item><n-card><n-statistic label="节点数" :value="status.nodes.length" /></n-card></n-grid-item>
      <n-grid-item><n-card><n-statistic label="直连可达" :value="direct" /></n-card></n-grid-item>
      <n-grid-item><n-card><n-statistic label="可用出口" :value="status.capabilities.length" /></n-card></n-grid-item>
      <n-grid-item><n-card><n-statistic label="运行时长(秒)" :value="status.uptime_secs" /></n-card></n-grid-item>
    </n-grid>

    <n-card title="本机" style="margin-top: 16px">
      <n-descriptions :column="2" label-placement="left" bordered>
        <n-descriptions-item label="名称">{{ status.self_node.name }}</n-descriptions-item>
        <n-descriptions-item label="ID">{{ status.self_node.id }}</n-descriptions-item>
        <n-descriptions-item label="地址">{{ status.self_node.ip }}:{{ status.listen_port }}</n-descriptions-item>
        <n-descriptions-item label="版本">v{{ status.version }}</n-descriptions-item>
        <n-descriptions-item label="cluster epoch">{{ status.cluster_epoch }}</n-descriptions-item>
        <n-descriptions-item label="固定端口">{{ status.pinned_port ?? '未固定' }}</n-descriptions-item>
      </n-descriptions>
    </n-card>

    <n-card title="内核" style="margin-top: 16px">
      <n-descriptions :column="2" label-placement="left" bordered>
        <n-descriptions-item label="状态">
          <n-tag :type="status.kernel.loaded ? 'success' : 'default'" size="small">
            {{ status.kernel.loaded ? '已加载' : '未加载' }}
          </n-tag>
        </n-descriptions-item>
        <n-descriptions-item label="版本">{{ status.kernel.version || '-' }}</n-descriptions-item>
        <n-descriptions-item label="rev">{{ status.kernel.rev }}</n-descriptions-item>
        <n-descriptions-item label="模块">
          {{ status.kernel.files.length ? status.kernel.files.join(', ') : '-' }}
        </n-descriptions-item>
      </n-descriptions>
    </n-card>
  </template>
</template>
