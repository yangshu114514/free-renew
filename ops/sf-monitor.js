// 三丰云 博客+龙虾 外部监控 (CF Worker classic, 从 CF 边缘探测 → PushPlus 直推微信)
// 探测: 博客(200) / 龙虾链路(401门=活) / 龙虾服务(Basic 深探, 非5xx=活)
// 告警: 状态翻转 + 30 分钟限频; GET /run 只检查, /run?push=1 推测试消息
// Source: https://developers.cloudflare.com/workers/configuration/cron-triggers/
const T = [
  { k: "博客", url: "https://REDACTED_BLOG_DOMAIN/", ok: [200] },
  { k: "龙虾链路", url: "https://REDACTED_GATEWAY_DOMAIN/", ok: [200, 401, 403, 302] }
]

async function probe(t, env) {
  let code = 0, err = ""
  const ctl = new AbortController()
  const timer = setTimeout(() => ctl.abort(), 25000)
  try {
    const r = await fetch(t.url, { method: "GET", redirect: "follow", signal: ctl.signal })
    code = r.status
  } catch (e) { err = String(e).slice(0, 60) }
  finally { clearTimeout(timer) }
  return { k: t.k, code, err, ok: t.ok.includes(code) }
}

async function probeSvc(env) {
  let code = 0, err = ""
  const ctl = new AbortController()
  const timer = setTimeout(() => ctl.abort(), 60000)
  try {
    const auth = "Basic " + btoa((env.OC_USER || "freerenew") + ":" + (env.OC_PASS || ""))
    const r = await fetch("https://REDACTED_GATEWAY_DOMAIN/v1/chat/completions", {
      method: "POST",
      headers: { "Content-Type": "application/json", "Authorization": auth },
      body: JSON.stringify({ model: "openclaw", messages: [{ role: "user", content: "ping" }], max_tokens: 1 }),
      redirect: "follow", signal: ctl.signal
    })
    code = r.status
  } catch (e) { err = String(e).slice(0, 60) }
  finally { clearTimeout(timer) }
  // 5xx/524(CF网关超时)/0 = 挂; 2xx/4xx = 活着(4xx = 认证层正常拒绝)
  return { k: "龙虾服务", code, err, ok: code > 0 && code < 500 }
}

async function monitor(env, force) {
  const rows = []
  for (const t of T) rows.push(await probe(t, env))
  if (env.OC_PASS) rows.push(await probeSvc(env))

  const failed = rows.filter(r => !r.ok).map(r => r.k + "(" + (r.code || "000") + (r.err ? "," + r.err : ""))
  const cache = await caches.open("sfmon")
  const KEY = "https://sfmon-cache.internal/state"
  const prevRes = await cache.match(KEY)
  const prev = prevRes ? await prevRes.json() : {}
  const now = Date.now()

  let pushList = []
  if (force) pushList.push({ title: "监控自检(手动): " + (failed.length ? "有异常 " + failed.join(",") : "全绿"), body: "<pre>" + JSON.stringify(rows, null, 1) + "</pre>" })
  if (failed.length && prev.state !== "down")
    pushList.push({ title: "❌ 三丰云监控掉线: " + failed.join(", "), body: "<pre>" + JSON.stringify(rows, null, 1) + "<br>北京时间 " + new Date().toLocaleString("zh-CN", { timeZone: "Asia/Shanghai" }) + "</pre>" })
  if (!failed.length && prev.state === "down")
    pushList.push({ title: "✅ 三丰云监控已恢复: 全绿", body: "<pre>" + JSON.stringify(rows, null, 1) + "</pre>" })

  const sinceLast = now - (prev.lastPush || 0)
  let pushed = 0
  // 限频: 非 force 时 30 分钟内最多推 1 条
  const doPush = pushList.filter(m => force || sinceLast > 18e5)
  for (const m of doPush) {
    let ppOk = false
    try {
      const r = await fetch("https://pushplus.plus/send", {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ token: env.PUSHPLUS_TOKEN, template: "html", title: m.title, content: m.body })
      })
      const j = await r.json().catch(() => ({}))
      ppOk = (j.code === 200 || j.code === 0)
      pushed += ppOk ? 1 : 0
    } catch (e) { pushed = -1 }
    // 双通道②: openclaw 龙虾代告 (仅龙虾服务层可达时; 挂掉则自然只剩 PushPlus)
    if (env.OC_PASS && rows.find(r2 => r2.k === "龙虾服务" && r2.ok)) {
      try {
        const auth = "Basic " + btoa((env.OC_USER || "freerenew") + ":" + env.OC_PASS)
        const msg = "监控告警转发: 用 message 工具 send 到微信 REDACTED_WECHAT_TARGET, 正文: " + m.title + "\n" + m.body.replace(/<pre>/g, "").replace(/<\/pre>/g, "") + " (PushPlus 已同步推送)。成功只回:已发送。"
        await fetch("https://REDACTED_GATEWAY_DOMAIN/v1/chat/completions", {
          method: "POST",
          headers: { "Content-Type": "application/json", "Authorization": auth },
          body: JSON.stringify({ model: "openclaw", messages: [{ role: "user", content: msg }], max_tokens: 200, stream: false })
        })
      } catch (e2) { /* 网关不可达 = 静默, PushPlus 已是主通道 */ }
    }
    if (!force) await cache.put(KEY, new Response(JSON.stringify({ state: failed.length ? "down" : "up", lastPush: now })))
  }
  return { rows, failed, pushed, at: new Date().toISOString() }
}

self.addEventListener("fetch", event => {
  event.respondWith((async req => {
    const at = req.headers.get("Authorization") || ""
    const u = new URL(req.url)
    // /oc: GHA → CF Worker → 网关 中转（绕开 CF 对数据中心 IP 的 managed challenge）
    // 鉴权与网关同一套 Basic(freerenew:OC_PASS)，Worker 验一次、网关再验一次
    if (u.pathname === "/oc") {
      const cors = {
        "access-control-allow-origin": "*",
        "access-control-allow-methods": "POST, OPTIONS",
        "access-control-allow-headers": "Authorization, Content-Type"
      }
      if (req.method === "OPTIONS") return new Response(null, { status: 204, headers: cors })
      const expect = "Basic " + btoa((globalThis.OC_USER || "freerenew") + ":" + (globalThis.OC_PASS || ""))
      if (at !== expect) return new Response("unauthorized", { status: 401, headers: cors })
      if (req.method !== "POST") return new Response("POST only", { status: 405, headers: cors })
      try {
        const body = await req.text()
        const ctl = new AbortController()
        const timer = setTimeout(() => ctl.abort(), 90000)
        const r = await fetch("https://REDACTED_GATEWAY_DOMAIN/v1/chat/completions", {
          method: "POST",
          headers: { "Content-Type": "application/json", "Authorization": at },
          body, redirect: "follow", signal: ctl.signal
        }).finally(() => clearTimeout(timer))
        const txt = await r.text()
        return new Response(txt, { status: r.status, headers: { "content-type": "application/json", ...cors } })
      } catch (e) {
        return new Response("gateway fetch error: " + String(e).slice(0, 200), { status: 502, headers: cors })
      }
    }
    // 公网入口鉴权: 所有 HTTP 路径要求 Authorization: Bearer <AUTH_TOKEN> (cron 定时不受此限)
    if (at !== "Bearer " + (globalThis.AUTH_TOKEN || "")) return new Response("unauthorized", { status: 401 })
    if (u.pathname === "/") return new Response("sf-monitor alive (GET /run to check)")
    if (u.pathname === "/t2") {
      try {
        const r = await fetch("https://REDACTED_BLOG_DOMAIN/", { redirect: "follow" })
        return new Response("blog fetch: " + r.status)
      } catch (e) {
        return new Response("blog fetch ERR: " + String(e).slice(0, 300))
      }
    }
    if (u.pathname === "/run") {
      try {
        const out = await monitor(globalThis, u.searchParams.has("push"))
        return new Response(JSON.stringify(out), { headers: { "content-type": "application/json" } })
      } catch (e) {
        return new Response("MON-ERR: " + (e && e.stack || String(e)).slice(0, 500))
      }
    }
    return new Response("not found", { status: 404 })
  })(event.request))
})

self.addEventListener("scheduled", event => {
  monitor(globalThis, false)
})
