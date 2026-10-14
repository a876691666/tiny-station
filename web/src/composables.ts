import { ref, onMounted, onUnmounted } from 'vue'
import { api, type StatusResponse } from './api'

/** Polls `/api/status` and exposes a reactive snapshot. */
export function useStatus(intervalMs = 5000) {
  const status = ref<StatusResponse | null>(null)
  const error = ref('')
  let timer: ReturnType<typeof setInterval> | undefined

  async function refresh() {
    try {
      status.value = await api.status()
      error.value = ''
    } catch (e) {
      error.value = String(e)
    }
  }

  onMounted(() => {
    refresh()
    timer = setInterval(refresh, intervalMs)
  })
  onUnmounted(() => {
    if (timer) clearInterval(timer)
  })

  return { status, error, refresh }
}
