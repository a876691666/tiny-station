<script setup lang="ts">
import { ref } from 'vue'
import { NCard, NInput, NButton, NSpace, NInputGroup, useMessage } from 'naive-ui'
import { api, type ProxyResult } from '../api'

const message = useMessage()
const url = ref('http://127.0.0.1:8000/')
const via = ref('auto')
const result = ref<ProxyResult | null>(null)
const busy = ref(false)

async function send() {
  busy.value = true
  try {
    result.value = await api.proxy(url.value, via.value || 'auto')
  } catch (e) {
    message.error(String(e))
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <n-card title="代理测试">
    <div class="sub">请求会经全网路由。via 填 auto 自动选择,或填节点名 / 节点 ID / 出口代理名。</div>
    <n-space vertical>
      <n-input-group>
        <n-input v-model:value="url" placeholder="https://example.com/api" />
        <n-input v-model:value="via" placeholder="auto" style="width: 200px" />
        <n-button type="primary" :loading="busy" @click="send">发送</n-button>
      </n-input-group>
      <template v-if="result">
        <div class="meta">
          HTTP <b>{{ result.status }}</b> · 处理节点 <code>{{ result.servedBy || '-' }}</code> · 路径
          <code>{{ result.path || '-' }}</code>
          <span v-if="result.error" class="err"> · 错误:{{ result.error }}</span>
        </div>
        <pre class="out">{{ result.body.slice(0, 6000) }}</pre>
      </template>
    </n-space>
  </n-card>
</template>

<style scoped>
.sub {
  color: #8b95a3;
  font-size: 12px;
  margin-bottom: 12px;
}
.meta {
  font-size: 13px;
}
.err {
  color: #e88080;
}
.out {
  background: #16181d;
  border: 1px solid #2a2f3a;
  border-radius: 6px;
  padding: 12px;
  max-height: 420px;
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-all;
}
</style>
