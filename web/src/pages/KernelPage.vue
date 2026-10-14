<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import {
  NCard,
  NButton,
  NSpace,
  NInput,
  NTag,
  NDescriptions,
  NDescriptionsItem,
  NDataTable,
  NTabs,
  NTabPane,
  useMessage,
  type DataTableColumns,
} from 'naive-ui'
import { api, ApiError, type KernelInfoResponse } from '../api'

const message = useMessage()
const info = ref<KernelInfoResponse | null>(null)
const code = ref('')
const spec = ref(
  '{\n  "entry": "main.lua",\n  "files": {\n    "main.lua": "local M={} M.abi=1 M.version=\\"v1\\"\\nreturn M\\n",\n    "lib/util.lua": "return {}\\n"\n  }\n}',
)
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

const apply = (bc: boolean) =>
  run(() => api.kernelPut(code.value, bc), bc ? '已推送全网' : '已本机升级')

function applyJson(bc: boolean) {
  let parsed: { entry?: string; files: Record<string, string> }
  try {
    parsed = JSON.parse(spec.value)
  } catch {
    message.error('JSON 解析失败')
    return
  }
  run(() => api.kernelPutJson(parsed, bc), bc ? '已推送全网' : '已本机升级')
}

async function loadSource() {
  try {
    code.value = await api.kernelSource()
  } catch {
    message.warning('尚无内核')
  }
}

const unload = () => run(() => api.kernelDelete(), '已卸载')
const reload = () => run(() => api.kernelReload(), '已重载')
const rollback = () => run(() => api.kernelRollback(), '已回滚')

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

  <n-card title="上传 / 热升级(免重启)">
    <n-tabs type="line" animated>
      <n-tab-pane name="single" tab="入口脚本">
        <n-space vertical>
          <div class="sub">粘贴入口脚本(main.lua),点「仅本机升级」热替换当前节点;「全网升级」推送到所有节点。</div>
          <n-input v-model:value="code" type="textarea" :rows="14" placeholder="粘贴 Luau 入口脚本(参考 examples/kernel/)" />
          <n-space>
            <n-button type="primary" :loading="busy" @click="apply(false)">仅本机升级</n-button>
            <n-button type="warning" :loading="busy" @click="apply(true)">全网升级</n-button>
            <n-button quaternary @click="loadSource">载入当前脚本</n-button>
          </n-space>
        </n-space>
      </n-tab-pane>

      <n-tab-pane name="multi" tab="多文件(JSON)">
        <n-space vertical>
          <div class="sub">整包替换模块集合:<code>{ entry, files: { "main.lua": "...", "lib/a.lua": "..." } }</code></div>
          <n-input v-model:value="spec" type="textarea" :rows="12" />
          <n-space>
            <n-button type="primary" :loading="busy" @click="applyJson(false)">仅本机升级</n-button>
            <n-button type="warning" :loading="busy" @click="applyJson(true)">全网升级</n-button>
          </n-space>
        </n-space>
      </n-tab-pane>
    </n-tabs>

    <n-space style="margin-top: 16px">
      <n-button :loading="busy" @click="reload">从磁盘重载</n-button>
      <n-button :loading="busy" @click="rollback">回滚上一版</n-button>
      <n-button type="error" :loading="busy" @click="unload">卸载内核</n-button>
    </n-space>
  </n-card>

  <n-card v-if="metricData.length" title="指标" style="margin-top: 16px">
    <n-data-table :columns="metricCols" :data="metricData" :bordered="false" size="small" />
  </n-card>
</template>

<style scoped>
.sub {
  color: #8b95a3;
  font-size: 12px;
  margin-bottom: 8px;
}
</style>
