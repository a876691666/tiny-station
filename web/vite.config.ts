import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// The dev server proxies the node's HTTP API so `pnpm dev` works against a
// locally running tiny-station (default port 45910).
const NODE = 'http://127.0.0.1:45910'

export default defineConfig({
  plugins: [vue()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    // Naive UI is imported per-component, so Rollup tree-shakes it automatically.
    target: 'es2020',
    rollupOptions: {
      output: {
        manualChunks: {
          naive: ['naive-ui'],
          vue: ['vue', 'vue-router'],
        },
      },
    },
  },
  server: {
    port: 5173,
    proxy: {
      '/api': NODE,
      '/proxy': NODE,
      '/x': NODE,
    },
  },
})
