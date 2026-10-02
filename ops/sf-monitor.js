// 三丰云 博客+龙虾 外部监控 (CF Worker classic, 从 CF 边缘探测 → PushPlus 直推微信)
// 探测: 博客(200) / 龙虾链路(401门=活) / 龙虾服务(Basic 深探, 非5xx=活)
// 告警: 状态翻转 + 30 分钟限频; GET /run 只检查, /run?push=1 推测试消息
// Source: https://developers.cloudflare.com/workers/configuration/cron-triggers/
//
// ⚠️ 监控目标与微信绑定 ID **不在本文件里**：这是公开仓库，具体域名/个人标识符
// 会直接暴露你的资产拓扑。三者从 Worker 的加密 secret 读取（由 deploy-worker.yml
// 从 GitHub Secrets 自动推送）：SF_BLOG_URL / SF_GATEWAY_URL / SF_WECHAT_TARGET。
// 未配置时该目标被跳过并计入 rows（带 MISSING 标记），监控仍然工作、只是少一路。
const T = [
  { k: "博客", envKey: "SF_BLOG_URL", ok: [200] },
  { k: "龙虾链路", envKey: "SF_GATEWAY_URL", ok: [200, 401, 403, 302] }
]

// 解析真实 URL：secret 未配置 → 返回 null（该探测项跳过，而不是拿空串去 fetch）
function targetUrl(t, env) {
  const v = (env[t.envKey] || "").trim()
  return v ? v : null
}

async function probe(t, env) {
  const url = targetUrl(t, env)
  if (!url) return { k: t.k + "(MISSING " + t.envKey + ")", code: 0, err: "secret 未配置", ok: false, skipped: true }
  let code = 0, err = ""
  const ctl = new AbortController()
  const timer = setTimeout(() => ctl.abort(), 25000)
  try {
    const r = await fetch(url, { method: "GET", redirect: "follow", signal: ctl.signal })
    code = r.status
  } catch (e) { err = String(e).slice(0, 60) }
  finally { clearTimeout(timer) }
  return { k: t.k, code, err, ok: t.ok.includes(code) }
}

async function probeSvc(env) {
  const gwUrl = (env.SF_GATEWAY_URL || "").trim()
  if (!gwUrl) return { k: "龙虾服务(MISSING SF_GATEWAY_URL)", code: 0, err: "secret 未配置", ok: false }
  let code = 0, err = ""
  const ctl = new AbortController()
  const timer = setTimeout(() => ctl.abort(), 60000)
  try {
    const auth = "Basic " + btoa((env.OC_USER || "") + ":" + (env.OC_PASS || ""))
    const r = await fetch(gwUrl.replace(/\/$/, "") + "/v1/chat/completions", {
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
    // 网关地址与微信绑定 ID 同样来自 Worker secret，不写进本文件。
    const gwUrl = (env.SF_GATEWAY_URL || "").trim()
    const wxTarget = (env.SF_WECHAT_TARGET || "").trim()
    if (env.OC_PASS && gwUrl && wxTarget && rows.find(r2 => r2.k === "龙虾服务" && r2.ok)) {
      try {
        const auth = "Basic " + btoa((env.OC_USER || "") + ":" + env.OC_PASS)
        const msg = "监控告警转发: 用 message 工具 send 到微信 " + wxTarget + ", 正文: " + m.title + "\n" + m.body.replace(/<pre>/g, "").replace(/<\/pre>/g, "") + " (PushPlus 已同步推送)。成功只回:已发送。"
        await fetch(gwUrl.replace(/\/$/, "") + "/v1/chat/completions", {
          method: "POST",
          headers: { "Content-Type": "application/json", "Authorization": auth },
          body: JSON.stringify({ model: "openclaw", messages: [{ role: "user", content: msg }], max_tokens: 200, stream: false })
        })
      } catch (e2) { /* 网关不可达 = 静默, PushPlus 已是主通道 */ }
    } else if (env.OC_PASS && gwUrl && !rows.find(r2 => r2.k === "龙虾服务" && r2.ok)) {
      /* 服务层本就挂了：不再试图转发，PushPlus 是唯一通道 */
    }
    if (!force) await cache.put(KEY, new Response(JSON.stringify({ state: failed.length ? "down" : "up", lastPush: now })))
  }
  return { rows, failed, pushed, at: new Date().toISOString() }
}

self.addEventListener("fetch", event => {
  event.respondWith((async req => {
    // 公网入口鉴权: 所有 HTTP 路径要求 Authorization: Bearer <AUTH_TOKEN> (cron 定时不受此限)
    const at = req.headers.get("Authorization") || ""
    const required = globalThis.AUTH_TOKEN || ""
    // AUTH_TOKEN **未绑定时必须拒绝，而不是放行**。
    // 旧写法 `at !== "Bearer " + (AUTH_TOKEN || "")` 在未配置时等价于
    // 只要请求头写成 `Bearer `（空值）就能通过——即 /run、/t2 对公网零门槛，
    // 任何人可白嫖 PushPlus 配额并触发网关 POST。未配置 = 拒绝（fail closed）。
    if (!required) return new Response("server not configured (AUTH_TOKEN unset)", { status: 503 })
    if (at !== "Bearer " + required) return new Response("unauthorized", { status: 401 })
    const u = new URL(req.url)
    if (u.pathname === "/") return new Response("sf-monitor alive (GET /run to check)")
    if (u.pathname === "/t2") {
      const blogUrl = (globalThis.SF_BLOG_URL || "").trim()
      if (!blogUrl) return new Response("blog fetch: SF_BLOG_URL secret 未配置", { status: 503 })
      try {
        const r = await fetch(blogUrl, { redirect: "follow" })
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
