import { createRouter, createWebHistory } from 'vue-router'

// Each page is a lazy-loaded chunk (Vite code-splitting) — only the visited
// page's JS is downloaded.
export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', name: 'dashboard', component: () => import('./pages/DashboardPage.vue') },
    { path: '/nodes', name: 'nodes', component: () => import('./pages/NodesPage.vue') },
    { path: '/config', name: 'config', component: () => import('./pages/ConfigPage.vue') },
    { path: '/proxy', name: 'proxy', component: () => import('./pages/ProxyPage.vue') },
    { path: '/kernel', name: 'kernel', component: () => import('./pages/KernelPage.vue') },
    { path: '/:pathMatch(.*)*', redirect: '/' },
  ],
})
