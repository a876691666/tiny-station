<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import {
  NCard,
  NButton,
  NSpace,
  NTag,
  NDescriptions,
  NDescriptionsItem,
  NDataTable,
  NUpload,
  NText,
  useMessage,
  type DataTableColumns,
  type UploadFileInfo,
} from 'naive-ui'
import { api, ApiError, type KernelInfoResponse } from '../api'

const message = useMessage()
const info = ref<KernelInfoResponse | null>(null)
const picked = ref<File | null>(null)
const busy = ref(false)

const errText = (e: unknown) => (e instanceof ApiError ? `${e.status} ${e.body}` : String(e))

async function loadInfo() {
  try {
    info.value = await api.kernelStatus()
  } catch (e) {
    message.error(errText(e))
  }
}
onMounted(loadInfo)

function onPick({ fileList }: { fileList: UploadFileInfo[] }) {
  picked.value = fileList[0]?.file ?? null
}

async function run(fn: () => Promise<unknown>, ok: string) {
  busy.value = true
  try {
    await fn()
    message.success(ok)
    await loadInfo()
  } catch (e) {
    message.error(errText(e))
  } finally {
    busy.value = false
  }
}

const apply = (broadcast: boolean) => {
  if (!picked.value) {
    message.warning('请先选择内核包(.zip)')
    return
  }
  const file = picked.value
  run(
    () => api.kernelPutPackage(file, broadcast),
    broadcast ? '已推送全网' : '已本机升级',
  )
}

const reload = () => run(() => api.kernelReload(), '已重载')
const rollback = () => run(() => api.kernelRollback(), '已回滚')
const unload = () => run(() => api.kernelDelete(), '已卸载')

const metricCols: DataTableColumns<{ name: string; value: number }> = [
  { title: '指标', key: 'name' },
  { title: '值', key: 'value' },
]
const metricData = computed(() =>
  Object.entries(info.value?.metrics ?? {}).map(([name, value]) => ({ name, value })),
)
</script>

<template>
  <n-card title="内核状态" style="margin-bottom: 16px">
    <n-descriptions :column="2" label-placement="left" bordered>
      <n-descriptions-item label="状态">
        <n-tag :type="info?.loaded ? 'success' : 'default'" size="small">
          {{ info?.loaded ? '已加载' : '未加载' }}
        </n-tag>
      </n-descriptions-item>
      <n-descriptions-item label="版本">{{ info?.version || '-' }}</n-descriptions-item>
      <n-descriptions-item label="ABI">{{ info?.abi ?? '-' }}</n-descriptions-item>
      <n-descriptions-item label="rev">{{ info?.rev ?? 0 }}</n-descriptions-item>
      <n-descriptions-item label="worker">{{ info?.workers ?? '-' }}</n-descriptions-item>
      <n-descriptions-item label="模块">{{ (info?.files ?? []).join(', ') || '-' }}</n-descriptions-item>
    </n-descriptions>
  </n-card>

  <n-card title="上传内核包(热升级,免重启)">
    <n-space vertical>
      <n-text depth="3" style="font-size: 12px">
        选择一个内核包(.zip,内含 main.lua 及可选 require 模块)。「仅本机升级」只更新当前节点;
        「全网升级」把该版本推送到所有节点并立即生效。
      </n-text>
      <n-upload :default-upload="false" :max="1" accept=".zip" @change="onPick">
        <n-button>选择内核包 (.zip)</n-button>
      </n-upload>
      <n-text v-if="picked" depth="3" style="font-size: 12px">已选择:{{ picked.name }}</n-text>
      <n-space>
        <n-button type="primary" :loading="busy" @click="apply(false)">仅本机升级</n-button>
        <n-button type="warning" :loading="busy" @click="apply(true)">全网升级</n-button>
      </n-space>
      <n-space>
        <n-button :loading="busy" @click="reload">从磁盘重载</n-button>
        <n-button :loading="busy" @click="rollback">回滚上一版</n-button>
        <n-button type="error" :loading="busy" @click="unload">卸载内核</n-button>
      </n-space>
    </n-space>
  </n-card>

  <n-card v-if="metricData.length" title="指标" style="margin-top: 16px">
    <n-data-table :columns="metricCols" :data="metricData" :bordered="false" size="small" />
  </n-card>
</template>
