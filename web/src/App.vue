<script setup lang="ts">
import { computed } from 'vue'
import { RouterView, useRoute, useRouter } from 'vue-router'
import {
  NConfigProvider,
  NMessageProvider,
  NLayout,
  NLayoutSider,
  NLayoutContent,
  NMenu,
  darkTheme,
  zhCN,
  dateZhCN,
  type MenuOption,
} from 'naive-ui'

const route = useRoute()
const router = useRouter()

const menu: MenuOption[] = [
  { label: '总览', key: '/' },
  { label: '节点', key: '/nodes' },
  { label: '组网配置', key: '/config' },
  { label: '代理测试', key: '/proxy' },
  { label: '内核', key: '/kernel' },
]

const active = computed(() => route.path)
const go = (key: string) => router.push(key)
</script>

<template>
  <n-config-provider :theme="darkTheme" :locale="zhCN" :date-locale="dateZhCN">
    <n-message-provider>
      <n-layout has-sider style="height: 100vh">
        <n-layout-sider bordered :width="200" :native-scrollbar="false">
          <div class="brand">tiny-station</div>
          <n-menu :value="active" :options="menu" @update:value="go" />
        </n-layout-sider>
        <n-layout>
          <n-layout-content
            :native-scrollbar="false"
            content-style="padding: 20px; max-width: 1200px"
            style="height: 100vh"
          >
            <router-view v-slot="{ Component }">
              <component :is="Component" />
            </router-view>
          </n-layout-content>
        </n-layout>
      </n-layout>
    </n-message-provider>
  </n-config-provider>
</template>

<style>
body {
  margin: 0;
  font-family: ui-monospace, Menlo, Consolas, 'Microsoft YaHei', monospace;
}
.brand {
  padding: 18px 20px;
  font-size: 15px;
  font-weight: 700;
  color: #63e2b7;
  letter-spacing: 0.05em;
}
</style>
