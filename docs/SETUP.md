# 安装指南（一键部署 free-renew）

> 目标：从零开始，10 分钟内让两台免费服务器进入自动续期状态，并可选配置通知渠道（出事时提醒你）。

## 前置条件

| 需要 | 说明 | 检查命令 |
|---|---|---|
| Rust 工具链 | 只在开发机需要；纯部署可跳过（GitHub Actions 每次运行时远程构建） | `cargo --version` |
| Git | 已登录（https 方式需 credential manager，ssh 需 key） | `git --version` |
| GitHub CLI（可选） | 直传 Secrets 用；没有就手动网页配置 | `gh auth status` |
| 云账号 | 阿贝云 / 三丰云 控制台账密 | — |
| CSDN 账号 | 已开通博客功能（新号先去 blog.csdn.net 完成开通） | — |
| LLM API | 任何 OpenAI 兼容接口（base_url + key + model） | — |

## 一键安装流程

### 第 1 步：Fork + Clone

```bash
# GitHub 网页上 Fork 本仓库到你名下（保持 Private！ Secrets 里有凭据）
git clone https://github.com/<你的用户名>/free-renew.git
cd free-renew
```

### 第 2 步：本地构建（可选，用于本地测试）

```bash
cargo build --release
./target/release/free-renew --help
```

### 第 3 步：填 Secrets

**方式 A：gh CLI（推荐）**

```bash
# 云账号（每家一条命令）
gh secret set SANFENGYUN_USERNAME --body "你的手机号"
gh secret set SANFENGYUN_PASSWORD --body "你的密码"
gh secret set ABEIYUN_USERNAME --body "你的手机号"
gh secret set ABEIYUN_PASSWORD --body "你的密码"

# LLM
gh secret set LLM_BASE_URL --body "https://api.example.com/v1"
gh secret set LLM_API_KEY --body "sk-..."
gh secret set LLM_MODEL --body "模型名"

# CSDN Cookie：用刷新脚本产出，见第 4 步；或手动：
gh secret set CSDN_COOKIES --body "UserName=xxx; UserToken=xxx; ..."
```

**方式 B：网页** → 仓库 Settings → Secrets and variables → Actions → New repository secret，逐条添加（字段对照见 config.example.toml 注释）。

### 第 4 步：采集 CSDN Cookie

```powershell
# Windows（推荐，专用 profile 保登录态，日常刷新零操作）
.\scripts\refresh-csdn-cookie.ps1
# 弹出浏览器 → 扫码登录 → 脚本自动导出 → 问你是否直传 Secret，按 y
```

Linux/macOS：`cargo run --release --bin csdn_cookie_export`（功能相同）。

### 选知乎发文路线（可选，替代 CSDN）

如果你 CSDN 号权重低、AI 文老被机审删除，可改用**知乎**（需一个发帖正常、有
权重的号）。三丰云/阿贝云认知乎文章 URL 作为续期凭证。

**1）切换平台**：仓库 Settings → Secrets and variables → **Variables** → New：

- 名字 `PLATFORM_PROVIDER`，值 `zhihu`
- （可选）名字 `ZHIHU_TOPICS`，值如 `云服务器 Linux`（空格分隔；不设则用默认）

**2）导出知乎 Cookie**（知乎没有采集脚本，手动复制，2 分钟）：

1. 电脑浏览器登录 <https://www.zhihu.com>
2. F12 → **Application（应用）** → Cookies → 选 `https://www.zhihu.com`
3. 把 `z_c0`、`_xsrf`、`d_c0`、`q_c1` 这几条复制拼成一行 `k=v; k=v; ...`
   （`z_c0` 是登录态命脉、`_xsrf` 发文必需，缺了会直接报错）
4. 存成 Secret：

```bash
gh secret set ZHIHU_COOKIES --body "z_c0=...; _xsrf=...; d_c0=...; q_c1=..."
```

**3）验证**：Actions 手动 Run 一轮，看是否走完"建草稿→挂话题→发布→截图→提交"。

> ⚠️ **账号风控风险（务必知情）**：本工具默认在 GitHub Actions 的微软数据中心
> IP 上、用你的知乎登录态自动发帖。这与"你本人平时登录的家用 IP"画像差异很大，
> 知乎风控可能弹出验证、限制发帖，严重时影响账号。**这是拿你的号在冒险**。
> 代码层面已做保护：一旦命中 401/403/验证码就**立刻停止且不重试**（反复撞风控
> 才会真把号搞封），并通过通知告知你。但"自动 + 机房 IP"这个根本画像的风险
> 无法由代码消除。若你的号很重要，优先考虑半自动（生成好草稿、你手动点发布）。

### 第 5 步：通知（可选但强烈建议）

没有通知 = 出了事你不知道。两种接法：

- **OpenClaw 用户**：参考下方"OpenClaw 网关通知"一节配置，微信直收
- **其他**：任意能收 POST JSON 的 webhook（Server酱、企业微信机器人、Bark…），
  填 `NOTIFY_WEBHOOK_URL`

### 第 6 步：点火验证

仓库 Actions 页 → 选 free-server-renewal → Run workflow。
第一次跑 = Linux 编译（~4 分钟）+ 真实登录查状态。绿了就完事，之后每天 09:30（北京时间）自动检查。

## OpenClaw 网关通知（微信直收）

硬性要求只有两条：

1. 跑着 OpenClaw 的服务器的 `/v1/chat/completions` 端点**能被 GitHub Actions 从公网访问到**——怎么暴露随你（公网 IP 直连、frp、Cloudflare Tunnel、其他隧道均可，下文步骤以作者自用的 Cloudflare Tunnel + 反代为例）
2. 端点**必须带认证**——暴露在公网的网关没有认证，任何人都相当于拿到了你 agent 的指令入口。basic auth 是最低要求（htpasswd 加专用 bot 用户，别用你本人的）

1. 网关 `openclaw.json` 开启：
   ```json
   "gateway": { "http": { "endpoints": { "chatCompletions": { "enabled": true } } } }
   ```
2. 反代给 `/v1/` 路径配 basic auth（认证由反代层实现；用其他暴露方式时，认证手段同理自选）
3. 给 agent 发消息拿微信 target：
   > "用 message 工具给微信发一条测试消息，告诉我你用的完整 target"
   > （形状 `xxxx@im.wechat`，**裸 ID，无 user: 前缀**——加前缀会 ret=-3）
4. Secrets 填三个：
   ```bash
   gh secret set NOTIFY_OPENCLAW_URL      --body "https://你的域名或IP:端口/v1/chat/completions"
   gh secret set NOTIFY_OPENCLAW_USER     --body "bot用户名"
   gh secret set NOTIFY_OPENCLAW_PASSWORD --body "bot密码"
   ```
5. 验证：本地设好三个环境变量（或写好 config.toml 的 [notify.openclaw] 段）后跑
   `./target/release/free-renew --test-notify`；
   也可以直接在 Actions 手动 Run 一轮，看微信是否收到"续期已提交"或失败告警。
   两种途径等价：Secrets（=Actions 环境变量）与 config.toml 均可驱动 OpenClaw 后端。

## Cookie 过期维护（唯一周期性人工任务）

**症状**：收到"CSDN 发文失败"通知（如已配置通知渠道），JSONL 日志里是 401/登录跳转。

**处置（30 秒）**：再跑一遍 `.\scripts\refresh-csdn-cookie.ps1`。专用 profile 里登录态通常还活着，脚本直接重新导出 → 按 y 直传 Secret → 完事。如果 profile 也过期了才需要重新扫码。

**知乎用户**：`ZHIHU_COOKIES` 里的 `z_c0` 失效（收"登录已过期/401"类通知）时，登录 zhihu.com → F12 重新复制完整 Cookie → `gh secret set ZHIHU_COOKIES --body "..."`。知乎 Cookie 无自动刷新脚本，只能手动。若通知是 403/风控字样，先别再重触发，按上文"风控"说明处理。

**预防**：CSDN Cookie 实测寿命数月。可以在日历上设个 2 个月提醒，或者干脆等通知来了再处理——失败当天就会尝试告警（需已配置通知渠道），不会静默丢失。

## 日常运维速查

| 症状 | 原因 | 处置 |
|---|---|---|
| Actions 红叉，没收到任何通知 | 通知后端挂了/没配 | 查 run-logs artifact 里的 JSONL；修通知后重跑 |
| 通知"发文失败" | CSDN Cookie 过期 | §Cookie 过期维护 |
| 知乎发文通知含 401/403/验证码 | z_c0 过期 或 触发知乎风控 | Cookie 过期就重复制；风控则停手、人工登录知乎解除，见"选知乎路线" |
| 知乎发文通知"挂话题失败" | 话题名匹配不到 | 改 `ZHIHU_TOPICS` 为常见话题（如"服务器"）；此项不致命，仍会尝试发布 |
| 通知"续期提交被拒" | 厂商审核拒绝（可能内容撞车/账号风控） | 看 JSONL 里 `raw` 字段的厂商原话；改 [ai].angles 换角度池 |
| 通知"提交异常 ret=-3"之类 | 通知指令问题 | 重跑 --test-notify；核对 target 规则 |
| 连续多天红叉 | 可能厂商改协议 | 提 issue / 对照 docs/protocol/ 手动复查端点 |
| 两台都到期但都成功 | 正常 | 每家 5 天窗口，run 里显示下次到期时间 |

## 安全清单（公开仓库部署者必读）

1. **永远保持 Fork 出来的仓库为 Private**——Secrets 虽然加密，但 Actions 日志可能包含厂商返回的账号信息
2. 云账号密码建议专用，不要和你其他账号复用
3. CSDN 账号同理；被风控了损失的是小号
4. 定期轮换密码（本仓库作者自己也是这么规划的）
5. LLM key 建议设余额上限

