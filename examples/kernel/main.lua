-- tiny-station 内核示例(多模块)
--
-- 内核是一个「模块集合」,不是单个文件:
--   main.lua            入口模块,返回内核表(必须含 abi / version,可选钩子)
--   lib/util.lua        工具模块(通过 require 引用)
--   routes/example.lua  路由 / 端点模块
--
-- require 是一个沙箱内的模块加载器:只解析内核自带的模块,支持 "lib.util"
-- 或 "lib/util" 两种写法,并缓存结果(标准 Lua 语义)。
--
-- 用法:
--   1) 放到配置同目录下的 .<配置名>.kernel/ 目录(默认),启动时自动加载;或
--   2) 单机热替换(无需重启):
--        curl -u admin:123456 -H 'content-type: application/json' \
--             --data @examples/kernel-bundle.json http://127.0.0.1:45910/api/kernel
--      或在网页管理界面「内核 · 热升级」粘贴 main.lua 后点「仅本机/全网升级」。
--   3) 全网签名分发:
--        tiny-station keygen
--        tiny-station sign examples/kernel --key <私钥> --rev 2 --version 0.3 > bundle.json
--        curl -u admin:123456 -H 'content-type: application/json' \
--             --data @bundle.json http://127.0.0.1:45910/api/kernel/distribute

local util = require("lib.util")
local routes = require("routes.example")

local M = {}

-- 必填:与宿主 ABI 匹配(当前为 1)。
M.abi = 1
-- 必填:版本字符串,用于观测。
M.version = "example-3"

-- 加载后调用一次。cfg 是本节点配置快照,ctx 是 host API。
function M.init(cfg, ctx)
  ctx.log.info("kernel init, node=" .. ctx.node.self().name)
  util.bump(ctx, "example:loads")
  ctx.timer.every(5000, function(ctx)
    ctx.metrics.gauge("example.peers", #ctx.node.peers())
  end)
end

function M.route(q, ctx)
  return routes.route(q, ctx)
end

function M.on_request(tunnel, ctx)
  return util.tag(tunnel, M.version)
end

function M.on_response(resp, ctx)
  return routes.on_response(resp, ctx)
end

M.endpoints = routes.endpoints

return M
