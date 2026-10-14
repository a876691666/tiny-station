-- 路由 / 端点模块:把策略与接口从入口模块里拆出来。
local M = {}

-- 路由决策:返回 { via = 节点名|id|"self", egress = 代理名 },返回 nil 用内置路由。
function M.route(q, ctx)
  if q.host == "internal.corp" then
    return { via = "self" }
  end
  if q.host == "api.blocked.example" then
    return { egress = "clash" }
  end
  return nil
end

function M.on_response(resp, ctx)
  if resp.status >= 500 then
    ctx.metrics.counter("example.upstream_5xx")
  end
  return nil
end

-- 自定义 HTTP 端点:通过 /x/<name> 访问(需要组网账号鉴权)。
M.endpoints = {
  ["/x/hello"] = function(req, ctx)
    return { status = 200, body = "hello from " .. ctx.node.self().name }
  end,
}

return M
