<script setup lang="ts">
import { ref, onMounted } from 'vue'
import {
  NCard,
  NForm,
  NFormItem,
  NInput,
  NInputNumber,
  NSwitch,
  NButton,
  NSpace,
  NGrid,
  NGridItem,
  NAlert,
  useMessage,
} from 'naive-ui'
import { api, type Config } from '../api'

const message = useMessage()
const cfg = ref<Config | null>(null)
const reachableText = ref('')
const saving = ref(false)

onMounted(async () => {
  try {
    cfg.value = await api.getConfig()
    reachableText.value = (cfg.value.reachable ?? []).join('\n')
  } catch (e) {
    message.error(String(e))
  }
})

const addPeer = () =>
  cfg.value?.peers.push({ address: '', name: '', username: '', password: '' })
const delPeer = (i: number) => cfg.value?.peers.splice(i, 1)
const addProxy = () => cfg.value?.proxies.push({ name: '', kind: 'http', url: '' })
const delProxy = (i: number) => cfg.value?.proxies.splice(i, 1)

async function save() {
  if (!cfg.value) return
  cfg.value.reachable = reachableText.value
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean)
  saving.value = true
  try {
    await api.saveConfig(cfg.value)
    message.success('已保存')
  } catch (e) {
    message.error(String(e))
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <n-alert v-if="!cfg" type="info" :show-icon="false">加载中…</n-alert>
  <template v-else>
    <n-card title="基本配置" style="margin-bottom: 16px">
      <n-form label-placement="left" label-width="150">
        <n-grid :cols="2" :x-gap="24">
          <n-grid-item>
            <n-form-item label="节点名称"><n-input v-model:value="cfg.name" /></n-form-item>
            <n-form-item label="对外 IP"><n-input v-model:value="cfg.ip" placeholder="自动探测" /></n-form-item>
            <n-form-item label="组网账号"><n-input v-model:value="cfg.api_username" /></n-form-item>
            <n-form-item label="组网密码"><n-input v-model:value="cfg.api_password" type="password" show-password-on="click" /></n-form-item>
          </n-grid-item>
          <n-grid-item>
            <n-form-item label="管理账号"><n-input v-model:value="cfg.admin_username" /></n-form-item>
            <n-form-item label="管理密码"><n-input v-model:value="cfg.admin_password" type="password" show-password-on="click" /></n-form-item>
            <n-form-item label="心跳间隔(秒)"><n-input-number v-model:value="cfg.gossip_interval_secs" :min="1" /></n-form-item>
            <n-form-item label="可访问外网"><n-switch v-model:value="cfg.internet" /></n-form-item>
          </n-grid-item>
        </n-grid>
        <n-form-item label="可直达主机/域名">
          <n-input v-model:value="reachableText" type="textarea" :rows="3" placeholder="esi.evetech.net&#10;127.0.0.1:39181&#10;192.168.1.0/24" />
        </n-form-item>
      </n-form>
    </n-card>

    <n-card title="组网目标服务" style="margin-bottom: 16px">
      <n-space vertical>
        <div v-for="(p, i) in cfg.peers" :key="i" class="row">
          <n-input v-model:value="p.address" placeholder="192.168.1.10" style="width: 200px" />
          <n-input v-model:value="p.name" placeholder="home" style="width: 140px" />
          <n-input v-model:value="p.username" placeholder="mesh" style="width: 120px" />
          <n-input v-model:value="p.password" type="password" placeholder="密码" style="width: 160px" />
          <n-button size="small" quaternary type="error" @click="delPeer(i)">删除</n-button>
        </div>
        <div>
          <n-button size="small" dashed @click="addPeer">+ 添加目标服务</n-button>
        </div>
      </n-space>
    </n-card>

    <n-card title="本机出口代理" style="margin-bottom: 16px">
      <n-space vertical>
        <div v-for="(p, i) in cfg.proxies" :key="i" class="row">
          <n-input v-model:value="p.name" placeholder="clash" style="width: 140px" />
          <n-input v-model:value="p.kind" placeholder="http" style="width: 120px" />
          <n-input v-model:value="p.url" placeholder="http://127.0.0.1:7890" style="width: 260px" />
          <n-button size="small" quaternary type="error" @click="delProxy(i)">删除</n-button>
        </div>
        <div>
          <n-button size="small" dashed @click="addProxy">+ 添加代理</n-button>
        </div>
      </n-space>
    </n-card>

    <n-button type="primary" :loading="saving" @click="save">保存配置</n-button>
  </template>
</template>

<style scoped>
.row {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-wrap: wrap;
}
</style>
