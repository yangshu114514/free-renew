// 三丰云博客 外部监控 (CF Worker, ES Module；从 CF 边缘探测 → PushPlus 直推微信)
// 探测: 博客站点 HTTP 200
// 告警: 状态翻转 + 30 分钟限频; GET /run 只检查, /run?push=1 推测试消息
//
// ── 2026-10-03 移除"龙虾（OpenClaw）"两路探测：本次误报的根治 ─────────────────
//
// 为什么必须移除：OpenClaw 的公网入口 shuyang.cc.cd 已被 owner 主动关停
// （2026-10-03 实测从公网 HTTP 000，完全不可达）。而本 Worker 跑在 Cloudflare
// **边缘**，物理上只能走公网——它没有任何办法通过服务器回环 127.0.0.1:18789
// 去看网关。于是每一轮都把"龙虾链路/龙虾服务"判死并推微信，成为一条**永远为真
// 的误报**（狼来了），把真正需要人看的告警淹没掉。
//
// 网关活性改由**服务器侧**看门狗负责（只有从 loopback 探测才看得到真相）：
//   · gw-watchdog.timer          每 30s 看 gateway 进程 RSS
//   · health-check.timer         每日 03:00 跑 agent 全面体检
//   · sanfengyun-watchdog.timer  每 10min 自愈（容器 / halo / 网络 / data-root）
// 边缘这层只回答"从外部看博客是否可达"——这才是边缘视角**能**回答的问题。
//
// 另修一处会制造永久误报的缺陷：旧代码把"secret 未配置"的跳过项也算进 failed，
// 于是漏配一个 secret 就等于每 10 分钟一次永久告警。现在 skipped 不计故障，
// 但**全部目标都被跳过**会单独告警（那代表监控实际处于失明状态，必须让人知道）。
//
// ⚠️ 监控目标不在本文件里（公开仓库）：从 Worker 加密 secret 读取（由
// deploy-worker.yml 从 GitHub Secrets 推送）：SF_BLOG_URL。
// ───────────────────────────────────────────────────────────────────────────
const T = [
  { k: "博客", envKey: "SF_BLOG_URL", ok: [200, 301, 302] }
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

async function monitor(env, force) {
  const rows = []
  for (const t of T) rows.push(await probe(t, env))

  // skipped（secret 未配置）**不算故障**——那是"没监控这个目标"，不是"目标挂了"。
  // 旧写法 rows.filter(r => !r.ok) 会把 MISSING 也判成故障，漏配即永久告警。
  const active = rows.filter(r => !r.skipped)
  const failed = active.filter(r => !r.ok).map(r => r.k + "(" + (r.code || "000") + (r.err ? "," + r.err : ""))
  // 全部目标都被跳过 = 监控实际失明：这必须报，否则"配置丢了"和"一切正常"长得一样
  const noTargets = active.length === 0
  const bad = failed.length > 0 || noTargets

  const cache = await caches.open("sfmon")
  const KEY = "https://sfmon-cache.internal/state"
  const prevRes = await cache.match(KEY)
  const prev = prevRes ? await prevRes.json() : {}
  const now = Date.now()

  const detail = "<pre>" + JSON.stringify(rows, null, 1) + "</pre>"
  const bjTime = "<br>北京时间 " + new Date().toLocaleString("zh-CN", { timeZone: "Asia/Shanghai" }) + "</pre>"
  let pushList = []
  if (force) pushList.push({
    title: "监控自检(手动): " + (bad ? (noTargets ? "无有效目标" : "有异常 " + failed.join(",")) : "全绿"),
    body: detail
  })
  if (bad && prev.state !== "down")
    pushList.push({
      title: noTargets ? "⚠️ 三丰云监控失明: 无有效目标（SF_BLOG_URL 未配置）" : "❌ 三丰云博客掉线: " + failed.join(", "),
      body: detail.replace(/<\/pre>$/, "") + bjTime
    })
  if (!bad && prev.state === "down")
    pushList.push({ title: "✅ 三丰云监控已恢复: 全绿", body: detail })

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
    if (!force) await cache.put(KEY, new Response(JSON.stringify({ state: bad ? "down" : "up", lastPush: now })))
  }
  return { rows, failed, noTargets, pushed, at: new Date().toISOString() }
}

export default {
  async fetch(request, env, ctx) {
    // 公网入口鉴权: 所有 HTTP 路径要求 Authorization: Bearer <AUTH_TOKEN> (cron 定时不受此限)
    const at = request.headers.get("Authorization") || ""
    const required = env.AUTH_TOKEN || ""
    // AUTH_TOKEN **未绑定时必须拒绝，而不是放行**。
    // 旧写法 `at !== "Bearer " + (AUTH_TOKEN || "")` 在未配置时等价于
    // 只要请求头写成 `Bearer `（空值）就能通过——即 /run、/t2 对公网零门槛，
    // 任何人可白嫖 PushPlus 配额。未配置 = 拒绝（fail closed）。
    if (!required) return new Response("server not configured (AUTH_TOKEN unset)", { status: 503 })
    if (at !== "Bearer " + required) return new Response("unauthorized", { status: 401 })
    const u = new URL(request.url)
    if (u.pathname === "/") return new Response("sf-monitor alive (GET /run to check)")
    if (u.pathname === "/t2") {
      const blogUrl = (env.SF_BLOG_URL || "").trim()
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
        const out = await monitor(env, u.searchParams.has("push"))
        return new Response(JSON.stringify(out), { headers: { "content-type": "application/json" } })
      } catch (e) {
        return new Response("MON-ERR: " + (e && e.stack || String(e)).slice(0, 500))
      }
    }
    return new Response("not found", { status: 404 })
  },

  async scheduled(event, env, ctx) {
    // 必须 waitUntil：不包的话定时任务会在探测/推送跑完之前被回收
    // （原 service-worker 写法就是只调用不等待，见文件头注释第 2 条）
    ctx.waitUntil(monitor(env, false))
  }
}
