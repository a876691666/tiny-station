<script setup lang="ts">
import { computed, h } from 'vue'
import { NCard, NDataTable, NTag, type DataTableColumns } from 'naive-ui'
import { useStatus } from '../composables'

const { status } = useStatus()

function stateTag(n: { is_self: boolean; directly_reachable: boolean; last_error?: string | null }) {
  if (n.is_self) return h(NTag, { type: 'info', size: 'small' }, { default: () => '本机' })
  if (n.directly_reachable) return h(NTag, { type: 'success', size: 'small' }, { default: () => '直连正常' })
  return h(NTag, { type: 'error', size: 'small' }, { default: () => n.last_error || '不可达' })
}

const nodeCols = computed<DataTableColumns<any>>(() => [
  { title: '节点', key: 'name' },
  { title: 'ID', key: 'id' },
  { title: '地址', key: 'addr' },
  { title: '状态', key: 'state', render: (r) => stateTag(r.raw) },
  { title: '延迟/原因', key: 'detail' },
  { title: '直连数', key: 'links' },
  { title: '外网', key: 'internet' },
  { title: '出口代理', key: 'proxies' },
  { title: '可直达', key: 'reachable' },
])

const nodeData = computed(() =>
  (status.value?.nodes ?? []).map((n) => ({
    key: n.info.id,
    name: n.info.name + (n.is_self ? ' (本机)' : ''),
    id: n.info.id,
    addr: `${n.info.ip}:${n.info.port}`,
    detail: n.is_self ? '运行中' : n.latency_ms != null ? `${n.latency_ms} ms` : '-',
    links: n.links.length,
    internet: n.internet ? '有' : '-',
    proxies: n.proxies.map((p) => `${p.name}(${p.kind})`).join(', ') || '-',
    reachable: n.reachable.join(', ') || '-',
    raw: n,
  })),
)

const routeCols: DataTableColumns<any> = [
  { title: '目标', key: 'dest' },
  { title: '跳数', key: 'hops' },
  { title: '路径', key: 'path' },
]
const routeData = computed(() =>
  (status.value?.routes ?? []).map((r) => ({
    key: r.dest_id,
    dest: r.dest_name,
    hops: r.hops,
    path: r.path.join(' → '),
  })),
)

const capCols: DataTableColumns<any> = [
  { title: '节点', key: 'node' },
  { title: '名称', key: 'name' },
  { title: '类型', key: 'kind' },
  { title: '地址', key: 'url' },
  { title: '范围', key: 'scope' },
]
const capData = computed(() =>
  (status.value?.capabilities ?? []).map((c) => ({
    key: `${c.node_id}:${c.name}`,
    node: c.node_name,
    name: c.name,
    kind: c.kind,
    url: c.url,
    scope: c.internet ? '可上外网' : '-',
  })),
)

const egressCols: DataTableColumns<any> = [
  { title: '名称', key: 'name' },
  { title: '类型', key: 'kind' },
  { title: '地址', key: 'url' },
  {
    title: '探测',
    key: 'alive',
    render: (r) =>
      h(
        NTag,
        { type: r.alive ? 'success' : 'error', size: 'small' },
        { default: () => (r.alive ? '可达' : '不可达') },
      ),
  },
]
const egressData = computed(() =>
  (status.value?.egress ?? []).map((e) => ({ key: e.name, ...e })),
)
</script>

<template>
  <n-card title="节点" style="margin-bottom: 16px">
    <n-data-table :columns="nodeCols" :data="nodeData" :bordered="false" size="small" />
  </n-card>
  <n-card title="本节点路由" style="margin-bottom: 16px">
    <n-data-table :columns="routeCols" :data="routeData" :bordered="false" size="small" />
  </n-card>
  <n-card title="全网可用出口代理" style="margin-bottom: 16px">
    <n-data-table :columns="capCols" :data="capData" :bordered="false" size="small" />
  </n-card>
  <n-card title="本机出口探测">
    <n-data-table :columns="egressCols" :data="egressData" :bordered="false" size="small" />
  </n-card>
</template>
