-- 工具模块:演示 ctx.store / ctx.metrics 与请求变换。
local M = {}

function M.bump(ctx, key)
  return ctx.store.incr(key)
end

-- 给出站请求打上内核标记头。
function M.tag(tunnel, version)
  tunnel.headers = tunnel.headers or {}
  tunnel.headers["x-tiny-station-kernel"] = version
  return tunnel
end

return M
