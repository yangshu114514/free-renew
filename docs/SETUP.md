# 安装与运维指南

从零到"两台免费服务器进入自动续期"，约 10 分钟。本文件是完整手册；日常最简路径看 README 即可。

## 工作原理（一句话）

GitHub Actions 每天定时拉起本仓库的 Rust 二进制：登录云厂商查续期状态，没到期几秒退出；到期则用 LLM 生成一篇体验文章 → 发布到**内容平台** → 浏览器截图 → 连同截图提交给厂商人工审核。成功/失败通过你配置的通知渠道告警（不配则仅 Actions 页可见）。

发文这一步支持 **CSDN** 和 **知乎** 两个平台，二选一，安装时选、之后可换。

## 前置条件

| 需要 | 说明 |
|---|---|
| Git（已登录 GitHub） | `gh auth login` 或 credential manager / SSH key |
| GitHub CLI（可选） | 装脚本/直传 Secrets 用；没有则全程网页手填 |
| 云账号 | 阿贝云 / 三丰云 控制台账密 |
| 发文平台账号 | CSDN（需已开通博客）**或** 知乎（发帖正常的号） |
| LLM API | 任何 OpenAI 兼容接口：base_url + key + model |
| Rust 工具链 | 仅本地开发/测试需要；纯部署可跳过（Actions 远程构建） |

## 推荐：一键安装向导（Windows）

```powershell
irm https://raw.githubusercontent.com/yangshu114514/free-renew/main/install.ps1 | iex
```

向导按 6 步问答完成全部配置：仓库 fork/clone → 云账号 Secrets → LLM → **选发文平台并采集其 Cookie** → 通知 → 定时与首跑。约 5 分钟。

下面手动流程与向导等价，供 Linux/macOS 或想逐步操作的人。

---

## 手动安装

### 1. Fork + Clone

在 GitHub 网页 **Fork** 本仓库到你名下，**保持 Private**（Secrets 存你名下；Public 会让 Actions 日志可能被公开，见安全清单）。

```bash
git clone https://github.com/<你的用户名>/free-renew.git
cd free-renew
```

（可选，本地测试才需要）`cargo build --release && ./target/release/free-renew --help`

### 2. 填云账号 + LLM Secrets

用 gh CLI（每条一个 Secret），或在 **Settings → Secrets and variables → Actions** 网页逐条添加。字段对照见 [config.example.toml](../config.example.toml)。

```bash
gh secret set SANFENGYUN_USERNAME --body "手机号"
gh secret set SANFENGYUN_PASSWORD --body "密码"
gh secret set ABEIYUN_USERNAME    --body "手机号"
gh secret set ABEIYUN_PASSWORD    --body "密码"
gh secret set LLM_BASE_URL        --body "https://api.example.com/v1"
gh secret set LLM_API_KEY         --body "sk-..."
gh secret set LLM_MODEL           --body "模型名"
```

> ⚠️ 用 gh 设 Secret 时，值必须用 `--body "实际值"` 直接给；`--body -` 会把 Secret 存成字面量 `-`（gh 只在完全不给 `--body` 时才读 stdin）。

### 3. 选发文平台（二选一）

平台由仓库 **Variable** `PLATFORM_PROVIDER` 决定（默认 `csdn`）：

```bash
gh variable set PLATFORM_PROVIDER --body "csdn"    # 或 zhihu
```

#### 平台 A：CSDN

1. 确保 CSDN 号已开通博客（先去 blog.csdn.net 完成开通）。
2. 采集 Cookie（会弹专用浏览器，扫码/登录后自动导出）：

```powershell
.\scripts\refresh-csdn-cookie.ps1     # Windows；结束时按 y 直传 Secret CSDN_COOKIES
```

Linux/macOS：`cargo run --release --bin csdn_cookie_export`，把打印的单行 Cookie 存进 Secret `CSDN_COOKIES`。

#### 平台 B：知乎

> ⚠️ **账号风险须知**：知乎路线在 GitHub Actions 的**机房 IP** 上、用你的登录态自动发帖，与"你本人家用 IP"画像差异大，可能触发风控/验证、严重时影响账号。**代码只能做到"命中验证码/403 即停且不重试"**，画像层面的风险无法由代码消除。号很重要就选 CSDN。

1. 切换平台：`gh variable set PLATFORM_PROVIDER --body "zhihu"`
2. 采集 Cookie：知乎登录态 `z_c0` 是 **httpOnly**，浏览器 F12 / `document.cookie` 抓不全，必须用脚本走 Chrome CDP 通道：

```powershell
.\scripts\refresh-zhihu-cookie.ps1    # 弹知乎登录页→登录→自动抓 z_c0→按 y 直传 Secret
```

备用手动法：F12 → **Network** → 刷新 → 点任一 zhihu.com 请求 → Request Headers → 复制整行 `Cookie:` → `gh secret set ZHIHU_COOKIES --body "z_c0=...; _xsrf=...; d_c0=...; q_c1=..."`。

3. 话题（可选）：`gh variable set ZHIHU_TOPICS --body "免费云服务器 虚拟主机"`（空格分隔，不设用此默认）。知乎发文通常必须挂话题，脚本会精确匹配并自动排除带其它云品牌名的话题。

### 4. 通知（可选，强烈建议）

没有通知 = 出事你不知道。二选一：

- **OpenClaw → 微信**（你有一台跑 OpenClaw 的服务器）：见下节。
- **通用 Webhook**（Server酱 / 企业微信机器人 / Bark…）：`gh secret set NOTIFY_WEBHOOK_URL --body "https://…"`

（完全不配也能跑，失败只体现在 Actions 页红叉。）

### 5. 首跑验证

知乎路线**强烈建议先探路**：Actions 页 Run workflow，`test_zhihu` 填 `三丰云`——只建草稿不发布，日志给出草稿编辑链接，你亲自核对内容质量与话题无误、且不触发验证码。确认干净后再正式用。

（想只看生成质量、连草稿都不建：`test_write` 填厂商名，样文打进日志。）

正式点火：Actions → free-server-renewal → Run workflow。首次含 Linux 编译约 4 分钟。绿了之后，按仓库 cron（默认每天 09:30 北京时间）自动检查；没到期几秒退出。

---

## OpenClaw 网关通知（微信直收）

硬性要求两条：① 服务器的 `/v1/chat/completions` 端点**能被 Actions 从公网访问**（公网直连 / frp / Cloudflare Tunnel / 其他隧道均可，示例用 Tunnel + 反代）；② 端点**必须带认证**（basic auth，htpasswd 建一个专用 bot 用户，别用你本人的——公网无认证等于把你的 agent 指令入口敞开）。

1. 网关配置开启：`"gateway": { "http": { "endpoints": { "chatCompletions": { "enabled": true } } } }`
2. 反代给 `/v1/` 加 basic auth（用其它暴露方式时认证手段同理自选）。
3. 拿微信 target：给 agent 发「用 message 工具给微信发一条测试消息，告诉我完整 target」。target 形状 `xxxx@im.wechat`，**裸 ID、无 `user:` 前缀**（加前缀会 ret=-3）。
4. 填三个 Secret：

```bash
gh secret set NOTIFY_OPENCLAW_URL      --body "https://你的域名或IP:端口/v1/chat/completions"
gh secret set NOTIFY_OPENCLAW_USER     --body "bot用户名"
gh secret set NOTIFY_OPENCLAW_PASSWORD --body "bot密码"
```

5. 验证：本地设好这三个环境变量（或写好 config.toml 的 `[notify.openclaw]`）后跑 `./target/release/free-renew --test-notify`，或直接 Actions 手动 Run 看是否收到微信。两种途径等价（Secrets 即 Actions 环境变量，与 config.toml 二选一即可）。

---

## 日常运维

### Cookie 过期（唯一周期性人工任务）

发文 Cookie 会过期（实测寿命数周到数月）。过期表现：收到"发文失败/401"类通知，或 Actions 红叉。

- **CSDN**：重跑 `.\scripts\refresh-csdn-cookie.ps1`（专用 profile 通常还在登录态，直接重导出，按 y 传 Secret）。
- **知乎**：重跑 `.\scripts\refresh-zhihu-cookie.ps1`。若通知是 403/风控/验证码，**别急着重触发**——先人工登录知乎解除验证，再刷新 Cookie。

### 速查表

| 症状 | 原因 | 处置 |
|---|---|---|
| Actions 红叉但没收到通知 | 通知未配置/挂了 | 查 run-logs artifact 的 JSONL；补通知后重跑 |
| 通知"发文失败 / 缺 _xsrf / 401" | 发文 Cookie 过期或没设对 | 重跑对应平台刷新脚本 |
| 知乎通知 403 / 验证码 | 触发知乎风控 | 停手，人工登录知乎解除，勿自动重试 |
| 知乎通知"挂话题失败/无安全匹配" | 话题名匹配不到或全带品牌名 | 换 `ZHIHU_TOPICS`（如"服务器"）；非致命，仍尝试发布 |
| 通知"续期提交被拒" | 厂商人工审核未过 | 看 JSONL `raw` 厂商原话；多为内容问题，可换角度池 |
| 通知"提交异常/登录超时" | 厂商 API 或跨洋线路抖动 | 通常下轮自动重来；连续多日再看 `*_LOGIN_URL` 走自建中继 |
| 连续多天红叉 | 可能厂商改了协议 | 对照 docs/protocol/ 手动复查端点，提 issue |

### 想换发文平台

改一个 Variable 即可，其余不动：`gh variable set PLATFORM_PROVIDER --body "zhihu"`（或 `csdn`），并确保对应平台的 Cookie Secret 已就绪。

---

## 手动触发与故障恢复

Actions 页 `Run workflow` 提供几个**诊断/恢复**输入框（都留空即走正常续期），按优先级：

| 输入 | 作用 | 何时用 |
|---|---|---|
| `test_write` | 只生成样文打进日志，**不发文/不碰任何平台** | 想看/调 LLM 生成质量 |
| `test_zhihu` | 真生成 + 在知乎**建草稿**（不发布），给编辑链接核对 | 上线前验鉴权/接口/话题/是否触发风控 |
| `test_screenshot_url` (+`_title`) | 只截图一个已发布 URL | 调截图/字体/标题匹配 |
| `submit_existing` (+`submit_vendor`/`submit_title`) | **复用一篇已发布文章**，只做 截图→提交厂商，**绝不重发** | 发文成功但"上传截图到厂商"被网络抖动掐断时，专攻重试、不再往平台灌新文 |

### 内容安全红线（为什么有时"生成失败/放弃本轮"）

生成端内置了**审核雷区一票否决**：翻墙/科学上网/内网穿透/frp/免备案/站群引流/灰产/政治等词一旦出现在文章里，直接判不合规→重写→连续违规则**放弃本轮、绝不发**（`src/writer.rs`）。这是 2026-09 一次真实删稿+账号被警告后加的护栏——**宁可这轮没续上，也不发一篇可能连累你账号的擦边文**。若你确信某正常用词被误杀，可在 `config.toml [ai] forbidden_words` 之外，反馈以调整词表。

### 发文→截图之间：会等文章"真正公开"

知乎/CSDN 刚发完的文章常处于**审核/放行延迟**，对抓取者短暂不可见。截图步骤会**轮询等待**（`ARTICLE_VISIBLE_TIMEOUT`，默认 720 秒）直到页面渲染出文章标题再截，避免把"登录墙/首页壳"当文章提交给厂商。若超时仍不可见 → 放弃本轮（不提交垃圾截图）。

---

## 安全清单

1. Fork 出的仓库**永远保持 Private**。
2. 云账号、发文平台账号建议专用小号，别和主账号复用。
3. LLM key 设余额上限。
4. 定期轮换密码与 Cookie。
5. **重要数据务必异地备份**——免费服务器随时可能因续期失败或政策变化丢失。

## 卸载

```powershell
.\uninstall.ps1            # 演练（默认）：只列出将删除的 Secrets/Variables/本地 profile，不删任何东西
.\uninstall.ps1 -Execute   # 真删：二次确认后清理本工具写入的 GitHub Secrets/Variables + 本地 cookie profile
```

不删：仓库/fork 本体、Actions 运行历史、厂商与内容平台的真实账号。定时任务请自行到 Actions 页 `Disable workflow`。
