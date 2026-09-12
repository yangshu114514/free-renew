# free-renew

[English](README.en.md) | 简体中文

阿贝云 / 三丰云「永久免费云服务器」自动续期。每天自动检查，到期自动完成「写文章 → 发文 → 截图 → 提交审核」全流程，出问题时通过配置的通知渠道（OpenClaw→微信、通用 Webhook 等）向你告警；未配置通知时，仅 Actions 页面可见。

> ⚠️ **免责声明**：本工具仅供学习与个人技术研究。厂商条款是否允许此类自动化由你自行判断，使用产生的一切后果（账号封禁/服务器回收/数据丢失）由使用者承担。完整条款见文末。

## 安装

本地已装 [Git](https://git-scm.com/) + [GitHub CLI](https://cli.github.com/)（`gh auth login` 过）即可：

```powershell
# Windows PowerShell —— 一键向导
irm https://raw.githubusercontent.com/yangshu114514/free-renew/main/install.ps1 | iex
```

向导分 6 步问答：仓库 → 云账号密码 → LLM API（可选测试）→ **选发文平台（CSDN/知乎）并采集其 Cookie** → 通知方式 → 每天几点跑 + 确认。全程约 5 分钟。想先安全预览可跑 `.\install.ps1 -DryRun`（不写任何东西）。

Linux/macOS 或不想用向导：clone 仓库后照 [docs/SETUP.md](docs/SETUP.md) 手动走一遍（内容相同，含两个发文平台的采集方式、可选的 OpenClaw 微信通知接线、故障速查表）。

## 发文平台：CSDN 或 知乎

续期文章要发到第三方内容平台供厂商审核，二选一，装时选、日后可换：

- **CSDN**（默认）：需已开通博客的 CSDN 号，Cookie 用 `scripts/refresh-csdn-cookie.ps1` 自动采集，最省心。
- **知乎**：需发帖正常的号；Cookie 用 `scripts/refresh-zhihu-cookie.ps1` 走 CDP 抓 httpOnly 的 `z_c0`。⚠️ 知乎在 Actions 机房 IP 上自动发帖有触发风控的实质风险，代码做到"弹验证码即停不重试"，但画像风险无法消除——号很重要请选 CSDN。

## 日常使用：只有一个命令

发文 Cookie 过期时（数周到数月一次；如配置了通知，会收到提醒）重跑对应平台的刷新脚本：

```powershell
.\scripts\refresh-csdn-cookie.ps1     # 用 CSDN
.\scripts\refresh-zhihu-cookie.ps1    # 用知乎
```

专用浏览器 profile 通常还保持登录态，脚本自动重新导出 Cookie 并可选直传 GitHub Secret，30 秒完事。

其他一切（每日检查、续期提交、失败告警、运行日志）全自动，无需关心。

要清理：`.\uninstall.ps1`（默认演练只列出、`-Execute` 才真删 GitHub Secrets/Variables 与本地 cookie profile）。采集脚本也支持 `.\scripts\refresh-*.ps1 -SelfTest` 只体检依赖与 C# 编译、不弹浏览器。

## 内容安全与故障恢复

- **内容安全红线**：生成端内置审核雷区一票否决（翻墙/内网穿透/免备案/灰产/政治等），命中就重写、连续命中则放弃本轮**绝不发**——宁可不续，也不发一篇可能连累你内容平台账号的擦边文（被真实删稿后加的护栏）。
- **发文→截图会等文章真正公开**：知乎/CSDN 刚发的文常短时不可见，截图步骤轮询等放行再截，不提交"登录墙"垃圾图。
- **`--submit-existing`**：发文成功却卡在"上传截图到厂商"的网络抖动时，用已发布文章**只重试截图+提交、不重发**，避免反复灌新文。
- 诊断入口（`--test-write` / `--test-zhihu` / 截图测试 / 上述恢复）见 [docs/SETUP.md](docs/SETUP.md)「手动触发与故障恢复」。

> 安装脚本支持 `-DryRun` 演练：`.\install.ps1 -DryRun` 只打印将执行的动作，不 fork、不写 Secret、不触发 workflow，可安全预览。

## 文档

| 文档 | 内容 |
|---|---|
| [docs/SETUP.md](docs/SETUP.md) | 完整安装指南、双发文平台、Secrets 明细、通知接线、日常运维速查表 |
| [docs/protocol/](docs/protocol/) | 技术细节：三丰云/阿贝云 `cmd=` 协议、CSDN 签名算法、知乎发文接口（实测样本） |
| [config.example.toml](config.example.toml) | 全部配置项及注释（LLM 词表/角度池/禁词均可自定义） |
| [NOTICE](NOTICE) | 第三方归属声明 |

架构一句话：**GitHub Actions 每天跑一次本仓库的 Rust 二进制**——登录云厂商查状态，没到期几秒退出；到期则 LLM 生成一篇随机角度、经禁词/必含词/AI 腔机器校验的体验文章，发布到所选内容平台（CSDN 或知乎），截图后提交给厂商审核，成功或失败都会通过已配置的通知渠道告警（OpenClaw→微信 / 通用 Webhook，可选）。60 天仓库无提交会导致定时任务被 GitHub 停用，已内置 [keepalive-workflow](https://github.com/marketplace/actions/keepalive-workflow) 自动保活。

## 致谢

本项目的协议层地基来自 **[BookerLiu/FreeServer](https://github.com/BookerLiu/FreeServer)**（Apache-2.0）——其作者在 2020 年逆向了阿贝云/三丰云的 API 协议，本项目沿用其端点与命令名并在 2026 年复测验证（不含其任何源代码，属独立再实现；归属声明见 [NOTICE](NOTICE)）。FreeServer 2021 年因"模板文章过不了人工审核"停更，本项目用 LLM 生成每篇不同的文章解决了这个死穴。感谢 Demo-Liu 的工作。

其他直接引用与致谢（完整清单见 [NOTICE](NOTICE)）：

- **[rust-headless-chrome](https://github.com/rust-headless-chrome/rust-headless-chrome)**（MIT）——Chrome DevTools Protocol 客户端。文章页截图的反检测能力（webdriver/chrome/plugins/permissions/webgl 五件套）来自其内置 `enable_stealth_mode()`。
- **[gautamkrishnar/keepalive-workflow](https://github.com/marketplace/actions/keepalive-workflow)**（MIT）——防止 GitHub 60 天无活动自动停用定时任务。
- **CSDN 签名常量**（x-ca-key / appSecret）出自 CSDN 前端 JS 内嵌的公开常量，社区解析见[腾讯云社区文章](https://cloud.tencent.com/developer/article/2420128)。
- **三丰云官方帮助文档**（content_1009 / content_1156）——审核红线条目的出处。
- **NodeLoc / CSDN 社区帖子**——厂商审核失败模式与免费服务器生态的情报来源。

## License

[Apache-2.0](LICENSE) · 版权 © 2026 yangshu114514 · 第三方归属见 [NOTICE](NOTICE)

**免责声明 / Disclaimer（中英全文）**：

1. 本项目仅供学习和个人技术研究使用。使用者应自行确认并遵守阿贝云、三丰云、CSDN、知乎及所使用的所有第三方平台的服务条款与使用规则。
2. 阿贝云、三丰云的免费服务器条款要求用户定期进行推广性质的续期操作。本项目对此类条款的自动化实现可能不被上述厂商认可或允许。**使用本项目产生的一切后果（包括但不限于账号封禁、服务器回收、数据丢失）由使用者自行承担。**
3. 本项目通过 LLM 生成文章内容。使用者应确保生成内容的发布符合所在平台的内容政策，并在平台上如实声明 AI 辅助生成（本项目默认开启该声明）。使用者不得利用本项目批量制造垃圾内容、刷量或从事其他滥用行为。
4. 本项目不存储、不上传、不收集任何用户凭据；所有配置仅保存在使用者本地或其私有仓库的 Secrets 中。使用者应妥善保管自己的凭据与 Cookie。
5. 免费服务器的稳定性与服务水平由厂商决定，本项目不对数据安全作任何担保。**请务必对重要数据做异地备份**——免费服务随时可能因续期失败、厂商政策变化等原因丢失。
6. 本项目按 Apache-2.0 许可证「原样」提供，作者不对任何直接或间接损失负责。继续使用即表示你已阅读、理解并同意上述全部条款。

English summary: for learning and personal research only; you are responsible for complying with all vendors' Terms of Service; any consequences (account suspension, data loss) are borne by the user; LLM-generated content must comply with platform policies and be truthfully declared; keep off-site backups of important data; provided "as is" under Apache-2.0 with no warranty.


