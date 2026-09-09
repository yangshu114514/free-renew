# free-renew

[English](README.en.md) | 简体中文

阿贝云 / 三丰云「永久免费云服务器」的自动续期工具。Rust 实现，单二进制，跑在 GitHub Actions 上，你自己的服务器零负载。

> **设计动机**：这两家厂商的免费服务器要求用户每隔几天在第三方平台发布推广文章并提交截图审核，未按时续期实例直接回收、数据清零。对学生和初创业者来说，这就是"哪几天没空管，服务器就没了"的定时炸弹。本项目把这颗炸弹拆掉：每天自动检查、到期自动完成"写文章 → 发文 → 截图 → 提交"全流程，任何一步出问题都会把原因推到你的微信上。

## 致谢与来源（Acknowledgments）

本项目**不是从零发明**，协议层的地基来自一个 2020 年的开源项目：

- **[BookerLiu/FreeServer](https://github.com/BookerLiu/FreeServer)**（Apache-2.0）——作者 Demo-Liu 在 2020 年逆向了阿贝云/三丰云的 `cmd=` 表单协议（`login.php` 登录、`renew.php` 的 `check_free_delay` / `free_delay_list` / `free_delay_add` 命令族），并以 Java + PhantomJS 实现了自动化。本项目的 API 端点、命令名、multipart 字段名（`yanqi_img`）均沿用其文档，并在 2026-09 对存活服务逐一复测验证（详见 [docs/protocol/](docs/protocol/)）。
- **本项目相对 FreeServer 的核心改进**：FreeServer 2021 年停止维护的直接原因是"发布平台审核越来越严，模板文章过不了人工审核"。本项目用 LLM 生成**每篇都不同**的使用体验文章（角度池随机 + 机器校验违规词），从根上解决内容重复问题；同时以 Rust 重写（单二进制 9MB，无 JVM/PhantomJS），把运行时挪到 GitHub Actions（原项目需要自己养一台常驻服务器，这本身就很好笑——用免费服务器续费免费服务器）。
- FreeServer 已于 2025-07 归档（read-only）。依照 Apache-2.0 的要求，其许可与归属信息保留于 [NOTICE](NOTICE) 文件；本项目不含其任何源代码，属于协议知识的独立再实现。

其他情报来源：CSDN `x-ca` 签名算法出自其前端 JS 与社区解析（[腾讯云社区文章](https://cloud.tencent.com/developer/article/2420128)），`x-ca-key` 与 `appSecret` 为 CSDN 前端内嵌的公开常量；三丰云审核规则出自其官方帮助文档（content_1009 / content_1156）。

## 工作原理

```
GitHub Actions cron（每天 09:30 北京时间，幂等）
  → 登录云厂商 API，查询免费服务器延期状态
  ├── 未到期 / 审核中 → 直接退出（< 1 分钟，零成本）
  └── 已到续期日 ↓
      → LLM 生成一篇使用体验文章（角度/字数随机，自动校验合规词表，不合格自动重写 ≤3 次）
      → 发文平台发布（CSDN Markdown 编辑器协议，HMAC-SHA256 签名）
      → 等文章页可访问 → headless Chrome 对文章页截图
      → multipart 提交 文章URL + 截图 到云厂商延期接口（free_delay_add）
      → 成功 / 失败 → OpenClaw 网关 → 微信通知
```

关键设计决策：

- **每次运行全新登录，不持久化任何 Cookie**。两家厂商会话有效期极短（阿贝云 5 天有效期的另一半含义），持久化只会带来"过期 Cookie"这种额外的故障模式——短命 Cookie 的正确解法是不存 Cookie。
- **fire-and-forget 通知**：通知经 OpenClaw 网关的 `chatCompletions` 端点触发 agent 在服务端异步执行。实测 Cloudflare 100 秒超时（HTTP 524）掐断客户端连接后，agent 依然完成微信投递——通知链路自身免疫网络抖动。
- **通知双后端**：OpenClaw（首选，送微信）与通用 JSON webhook（备用），配置切换；通知失败绝不阻塞续期主流程（`notify.rs` 全部错误就地吞掉只记日志）。
- **宁缺毋滥**：LLM 生成 3 轮仍不合规（含禁词/缺关键词/太短）则放弃本轮。5 天窗口 + 每天重试，浪费一轮的代价远小于提交垃圾内容被拉黑。

## 已验证的协议（2026-09）

完整协议细节、实测请求/响应样本、签名算法逐字段说明见 [docs/protocol/](docs/protocol/) 三份文档。此处只列结论：

| 环节 | 端点 | 验证结论 |
|---|---|---|
| 三丰云登录/状态/提交 | `api.sanfengyun.com/www/{login,renew}.php` | ✅ 真账号全通。纯 Cookie + 表单，无 token 无签名无 CSRF（来自官方控制台 SPA 的 axios 配置实锤） |
| 阿贝云登录/状态/提交 | `api.abeiyun.com/www/{login,renew}.php` | ✅ 真账号通。**境外 IP 必须 HTTP 80 降级**（HTTPS 被 WAF 拦，且 DNS 有假 AAAA 记录 `fc00::6`） |
| CSDN 发文 | `bizapi.csdn.net/blog-console-api/v3/mdeditor/saveArticle` | ✅ 真账号发文成功。HMAC-SHA256 签名，**待签串 accept 与 content-type 之间是双空行**（社区流传的 Java 复现版少一个 `\n`，是错的） |

两家厂商的响应字段形状**不同且都在变**（三丰云 `delay_state` 是中文状态字如"审核中"，阿贝云 `delay_enable` 是 JSON 数字），解析逻辑按厂商分开实现并锁定实测样本（`src/cloud.rs::parse_state`），这是踩过坑的位置，别合并它们。

一个悬案：社区报告 `renew.php` 会返回 `50220` 错误（疑似账号锁定专属）。本项目未登录态探测只见过 `50140`（尚未登录），真账号从未复现 50220。若你的账号被锁，大概率会撞上它——届时本项目的 JSONL 日志会留下完整现场。

## LLM 文章生成规则

生成与校验是两道独立的关：

**生成时喂给模型的约束**（`src/writer.rs`）：

- 人设：个人博客技术博主，文风自然、有具体操作细节或数字；**明确禁止**营销腔、AI 味排比句、"首先/其次/总之"模板结构、"个人观点"式免责声明、求点赞关注式结尾
- 角度池 8 选 1 随机：新手踩坑 / 横向对比 / 部署实录 / 性能网络实测 / 学生党视角 / 系统重装记录 / 建站选型思考 / 控制台与工单体验
- 字数池 4 档随机（300/380/420/500 字），temperature 1.0
- 必含：厂商名、"免费云服务器"、"免费虚拟主机"、官网域名融入正文
- 禁词表：申请延期 / 延期申请 / 续期 / 续费 / 白嫖 / 薅羊毛
- 要求穿插 1-2 处"（配图：xxx）"占位说明——对口三丰云"图文并茂优先通过"的审核条款

**生成后的机器校验**（不过就带着违规点重写，最多 3 轮）：

1. 厂商名出现 ✓
2. 每个必含关键词出现 ✓
3. 官网链接出现 ✓
4. 禁词表零命中 ✓
5. 正文 ≥ 150 字 ✓

以上全部可通过 `config.toml [ai]` 节覆盖（角度池、字数池、禁词表、必含词、重试次数）——**建议按你自己的文风改写角度池**，这让每篇文章更个性化，也更像人写的。

**发布侧合规**：CSDN `creation_statement` 默认 `1`（"部分内容由 AI 辅助生成"声明，展示在文章页）。诚实优先；改为 `0` 的合规风险自担。

## 快速开始

### 1. 本地构建

```bash
cargo build --release
# 产物 ~9MB 单二进制（lto=fat + codegen-units=1 + strip + panic=abort）
```

### 2. 准备配置

```bash
cp config.example.toml config.toml
# 编辑 config.toml，填入云账号、CSDN Cookie、LLM 配置
```

CSDN Cookie 采集（本地有 Chrome，扫码登录一次，自动检测登录态并导出，无需按回车）：

```bash
cargo run --release --bin csdn_cookie_export
# 产出 csdn_cookies.txt（Netscape 全文）+ csdn_cookies_oneline.txt（Secret 用的单行版）
```

### 3. 运行

```bash
./target/release/free-renew                        # 读 ./config.toml
./target/release/free-renew --config /path/to.toml # 显式指定
./target/release/free-renew --test-notify          # 只测通知链路（发一条到微信），不碰云厂商
```

### 4. GitHub Actions 部署（推荐）

1. Fork 本仓库或推到你的**私有仓库**
2. 配置仓库 Secrets（与 config.toml 对应的环境变量，优先级高于文件）：

| Secret | 说明 |
|---|---|
| `SANFENGYUN_USERNAME` / `SANFENGYUN_PASSWORD` | 三丰云账号（手机号+密码） |
| `ABEIYUN_USERNAME` / `ABEIYUN_PASSWORD` | 阿贝云账号 |
| `LLM_BASE_URL` / `LLM_API_KEY` / `LLM_MODEL` | OpenAI 兼容接口（任何家都行） |
| `CSDN_COOKIES` | CSDN 单行 Cookie（采集器产出） |
| `NOTIFY_OPENCLAW_URL` / `NOTIFY_OPENCLAW_USER` / `NOTIFY_OPENCLAW_PASSWORD` | 可选，OpenClaw 网关通知（→微信） |

3. Actions 页面手动 `workflow_dispatch` 触发一次验证，之后每天定时自动运行
4. 每轮运行的 JSONL 详细日志自动上传为 `run-logs` artifact（保留 30 天）

配置优先级：**环境变量 > config.toml > 内置默认值**。Actions 场景全部走 Secrets，本地开发走文件，同一套代码两种喂法。

## 项目结构

```
src/
├── main.rs           # 流程编排：查状态 → 生成 → 发文 → 截图 → 提交（幂等）
├── config.rs         # 运行时配置（文件 + 环境变量合并）
├── file_config.rs    # config.toml schema
├── cloud.rs          # 云厂商 API（登录/状态/提交；双厂商解析差异在此，别合并）
├── csdn.rs           # CSDN 发文（HMAC-SHA256 签名 + saveArticle + md→html）
├── writer.rs         # LLM 文章生成 + 合规校验循环
├── screenshot.rs     # headless Chrome 截图 + 文章就绪轮询
├── notify.rs         # 通知（openclaw / webhook 双后端，尽力而为不阻塞主流程）
├── logging.rs        # JSONL 运行日志（每步耗时/结果/原始响应摘录，凭据脱敏）
├── http.rs           # 公共常量
└── bin/
    └── csdn_cookie_export.rs  # CSDN Cookie 采集器（本地交互式扫码）
docs/protocol/        # 三丰云/阿贝云/CSDN 协议全文（实测样本 + 签名算法）
.github/workflows/    # Actions 工作流（Linux 编译运行 + 缓存 + 日志 artifact）
```

技术栈：Rust（reqwest blocking / headless_chrome / git2 已移除 / hmac+sha2+base64 / toml / tracing）。全程序串行无 tokio——流程严格线性依赖上一步结果，blocking API 让错误处理和测试都简单一截。

## 已知限制与悬案

- **50220**：见上文，未复现，原因不明
- **CSDN Cookie 寿命**：数月。过期后发文失败 → 微信通知 → 重新扫码 1 分钟。这是全系统唯一需要人工的周期性维护项
- **微信通知依赖 OpenClaw 网关**：如果你的 OpenClaw 挂了，通知静默丢失（日志里有记录），但续期主流程照跑
- **阿贝云审核**：目前只验证了 API 提交通畅，人工审核通过率有待长期数据（三丰云侧历史通过率 100%，10/11 条，唯一失败是一次文章被平台删除）
- **知乎路线**：未实现（无 API，签名算法 `x-zse-96` 复杂）。你两家厂商的历史记录证明知乎专栏人工发帖 100% 过审，如果 CSDN 被风控，这是备用人工路线
- **README 中的平台名**：仅作技术描述，与上述厂商无任何关联，见免责声明

## 免责声明 / Disclaimer

**中文（以此为准）**：

1. 本项目仅供学习和个人技术研究使用。使用者应自行确认并遵守阿贝云、三丰云、CSDN 及所使用的所有第三方平台的服务条款与使用规则。
2. 阿贝云、三丰云的免费服务器条款要求用户定期进行推广性质的续期操作。本项目对此类条款的自动化实现可能不被上述厂商认可或允许。**使用本项目产生的一切后果（包括但不限于账号封禁、服务器回收、数据丢失）由使用者自行承担。**
3. 本项目通过 LLM 生成文章内容。使用者应确保生成内容的发布符合所在平台的内容政策，并在平台上如实声明 AI 辅助生成（本项目默认开启该声明）。使用者不得利用本项目批量制造垃圾内容、刷量或从事其他滥用行为。
4. 本项目不存储、不上传、不收集任何用户凭据；所有配置仅保存在使用者本地或其私有仓库的 Secrets 中。使用者应妥善保管自己的凭据与 Cookie。
5. 免费服务器的稳定性与服务水平由厂商决定，本项目不对数据安全作任何担保。**请务必对重要数据做异地备份**——免费服务随时可能因续期失败、厂商政策变化等原因丢失。
6. 本项目按 Apache-2.0 许可证「原样」提供，作者不对任何直接或间接损失负责。继续使用即表示你已阅读、理解并同意上述全部条款。

**English**:

1. This project is for learning and personal technical research only. Users are responsible for confirming and complying with the Terms of Service of Abeiyun, Sanfengyun, CSDN, and all other third-party platforms involved.
2. The free-server terms of Abeiyun and Sanfengyun require periodic promotion-style renewal operations. Automating such terms may not be endorsed or permitted by these vendors. **Any consequence of using this project (including but not limited to account suspension, server reclamation, or data loss) shall be borne by the user.**
3. Article content is generated by LLMs. Users must ensure generated content complies with the content policies of the publishing platform, and truthfully declare AI-assisted generation (enabled by default). Users must not use this project to mass-produce spam, inflate metrics, or engage in other abuse.
4. This project does not store, upload, or collect any user credentials; all configuration lives in your local files or your private repository's Secrets. Keep your credentials and cookies safe.
5. The stability of free servers is decided by the vendors. This project makes NO warranty of data safety. **Always keep off-site backups of important data** — free servers may be lost at any time due to failed renewal or policy changes.
6. This project is provided "as is" under the Apache-2.0 license. The authors are not liable for any direct or indirect damages. Continued use constitutes that you have read, understood, and agreed to all of the above.

## License

Apache-2.0，版权声明见 [LICENSE](LICENSE)，第三方归属见 [NOTICE](NOTICE)。
