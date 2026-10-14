#!/usr/bin/env sh
# tiny-station 数据面客户端(shell / curl)。
#
# 全部参数都在 JSON body 里,不占用 query,因此没有 URL 长度限制。
# 依赖:curl(必需)、jq(用于拼 JSON 与解码,建议安装)。
#
# 直接运行:
#   NODE=http://192.168.1.10:45910 TS_USER=mesh TS_PASS=123456 ./request.sh https://api.example.com/v1/items
#
# 作为库 source 后使用:
#   . ./request.sh
#   ts_get  "https://api.example.com/v1/items?page=2"
#   ts_post "http://192.168.1.66:8080/api/orders" '{"sku":"A-1","qty":2}'
#   ts_get  "https://api.example.com/v1/items" | jq -r '.body' | base64 -d

NODE="${NODE:-http://127.0.0.1:45910}"
TS_USER="${TS_USER:-mesh}"
TS_PASS="${TS_PASS:-123456}"

# ts_request <json-envelope>  —— 发送请求信封,返回原始 JSON 响应
ts_request() {
  curl -fsS "$NODE/api/request" \
    -u "$TS_USER:$TS_PASS" \
    -H 'content-type: application/json' \
    --data "$1"
}

# ts_get <url> [via]
ts_get() {
  ts_request "$(jq -cn --arg u "$1" --arg v "${2:-auto}" \
    '{url:$u, method:"GET", via:$v}')"
}

# ts_post <url> [json-body] [via]
ts_post() {
  _body="${2:-}"
  [ -n "$_body" ] || _body='{}'
  ts_request "$(jq -cn --arg u "$1" --arg v "${3:-auto}" --argjson b "$_body" \
    '{url:$u, method:"POST", via:$v, body:$b}')"
}

# 仅当被直接执行(而非 source)时跑一个演示
if [ "${0##*/}" = "request.sh" ]; then
  set -eu
  ts_get "${1:-${URL:-http://127.0.0.1:8000/}}" | jq .
fi
