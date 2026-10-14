// Typed client for the tiny-station HTTP API. Auth is HTTP Basic, cached by the
// browser after the initial page load, so same-origin fetches reuse it.

export interface NodeInfo {
  id: string
  name: string
  ip: string
  port: number
  version: string
}

export interface ProxyInfo {
  name: string
  kind: string
  url: string
}

export interface NodeStatus {
  info: NodeInfo
  is_self: boolean
  directly_reachable: boolean
  latency_ms: number | null
  last_seen_ms: number
  links: string[]
  proxies: ProxyInfo[]
  reachable: string[]
  internet: boolean
  last_error?: string | null
}

export interface RouteEntry {
  dest_id: string
  dest_name: string
  next_hop_id: string
  next_hop_name: string
  hops: number
  path: string[]
}

export interface Capability {
  node_id: string
  node_name: string
  kind: string
  name: string
  url: string
  internet: boolean
}

export interface EgressStatus {
  name: string
  kind: string
  url: string
  alive: boolean
}

export interface KernelStatus {
  loaded: boolean
  version: string
  rev: number
  workers: number
  source: string
  files: string[]
}

export interface StatusResponse {
  self_node: NodeInfo
  version: string
  listen_port: number
  pinned_port?: number | null
  ports: number[]
  cluster_epoch: number
  uptime_secs: number
  nodes: NodeStatus[]
  routes: RouteEntry[]
  capabilities: Capability[]
  egress: EgressStatus[]
  kernel: KernelStatus
}

export interface PeerConfig {
  address: string
  name?: string | null
  username?: string | null
  password?: string | null
}

export interface ProxyConfig {
  name: string
  kind: string
  url: string
}

export interface Config {
  name?: string | null
  ip?: string | null
  bind?: string | null
  port?: number | null
  api_username?: string | null
  api_password?: string | null
  admin_username?: string | null
  admin_password?: string | null
  mesh_key?: string | null
  internet: boolean
  gossip_interval_secs?: number | null
  reachable: string[]
  peers: PeerConfig[]
  proxies: ProxyConfig[]
  [k: string]: unknown
}

export interface KernelInfoResponse {
  loaded: boolean
  abi?: number
  version?: string
  workers?: number
  source?: string
  rev: number
  metrics: Record<string, number>
  files: string[]
}

export interface ProxyResult {
  status: number
  servedBy: string
  path: string
  error: string
  body: string
}

export class ApiError extends Error {
  status: number
  body: string
  constructor(status: number, body: string) {
    super(`${status} ${body}`)
    this.status = status
    this.body = body
  }
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch(path, { credentials: 'same-origin', ...init })
  if (!r.ok) {
    const body = await r.text().catch(() => '')
    throw new ApiError(r.status, body || r.statusText)
  }
  const ct = r.headers.get('content-type') || ''
  return (ct.includes('json') ? await r.json() : await r.text()) as T
}

function json(body: unknown): RequestInit {
  return { headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }
}

export const api = {
  status: () => call<StatusResponse>('/api/status'),

  getConfig: () => call<Config>('/api/config'),
  saveConfig: (cfg: Config) =>
    call<{ ok: boolean }>('/api/config', { method: 'POST', ...json(cfg) }),

  kernelStatus: () => call<KernelInfoResponse>('/api/kernel'),
  kernelSource: () => call<string>('/api/kernel/source'),
  kernelPut: (code: string, broadcast: boolean) =>
    call<{ ok: boolean }>(`/api/kernel?broadcast=${broadcast}`, {
      method: 'POST',
      headers: { 'content-type': 'text/plain; charset=utf-8' },
      body: code,
    }),
  kernelPutJson: (spec: { entry?: string; files: Record<string, string> }, broadcast: boolean) =>
    call<{ ok: boolean }>(`/api/kernel?broadcast=${broadcast}`, {
      method: 'POST',
      ...json(spec),
    }),
  async kernelPutPackage(file: File, broadcast: boolean) {
    const buf = await file.arrayBuffer()
    return call<{ ok: boolean; version?: string; modules?: number }>(
      `/api/kernel?broadcast=${broadcast}`,
      { method: 'POST', headers: { 'content-type': 'application/zip' }, body: buf },
    )
  },
  kernelReload: () => call<{ ok: boolean }>('/api/kernel/reload', { method: 'POST' }),
  kernelRollback: () => call<{ ok: boolean }>('/api/kernel/rollback', { method: 'POST' }),
  kernelDelete: () => call<{ ok: boolean }>('/api/kernel', { method: 'DELETE' }),

  async proxy(target: string, via: string): Promise<ProxyResult> {
    const r = await fetch(
      `/proxy?target=${encodeURIComponent(target)}&via=${encodeURIComponent(via)}`,
      { credentials: 'same-origin' },
    )
    return {
      status: r.status,
      servedBy: r.headers.get('x-tiny-station-served-by') || '',
      path: r.headers.get('x-tiny-station-path') || '',
      error: r.headers.get('x-tiny-station-error') || '',
      body: await r.text(),
    }
  },
}
