# 知乎发文接口（实测 2026-09）

知乎专栏发文走的是编辑器后台的 Web 接口，非官方开放 API。本项目实现见 `src/zhihu.rs`，
链路移植自社区成熟工具（zimya/zhihu_obsidian 等）的纯 HTTP 流程。

## 鉴权

写接口只需浏览器 Cookie，**不强制 x-zse-96 签名**（实测建草稿/写正文/挂话题/发布四步，
带正确 Cookie + `x-xsrftoken` + `x-requested-with: fetch` 即通过）。关键 Cookie：

| Cookie | 作用 |
|---|---|
| `z_c0` | 登录态命脉（**httpOnly**，`document.cookie` 抓不到，须 CDP/Network 面板取） |
| `_xsrf` | CSRF token，同时作为 `x-xsrftoken` 请求头发送 |
| `d_c0` | 设备指纹 |
| `q_c1` | 常与上面配合 |

必需请求头：`Cookie`、`x-requested-with: fetch`、`x-xsrftoken: <_xsrf>`、
`Origin: https://zhuanlan.zhihu.com`、`Referer: https://zhuanlan.zhihu.com/write`、
常规浏览器 `User-Agent`。

## 发文四步

1. **建草稿** `POST https://zhuanlan.zhihu.com/api/articles/drafts`
   body `{"title","delta_time":0,"can_reward":false}` → 返回 `{"id": <draft_id>}`
2. **写正文** `PATCH https://zhuanlan.zhihu.com/api/articles/{id}/draft`
   body `{"title","content"(HTML),"table_of_contents","delta_time","can_reward"}`
3. **挂话题**（不挂通常发不出去）
   - 补全 `GET /api/autocomplete/topics?token=<词>&max_matches=5&use_similar=0&topic_filter=1`
   - 绑定 `POST /api/articles/{id}/topics`，body 为补全结果里的单个话题对象
4. **发布** `POST https://www.zhihu.com/api/v4/content/publish`
   body `{"action":"article","data":{"publish":{...},"draft":{"id":...,"isPublished":false},"extra_info":{...}}}`
   → `{"message":"success", ...}`；公开地址 `https://zhuanlan.zhihu.com/p/{id}`

## 话题匹配陷阱（重要）

`autocomplete/topics` 返回按热度排，**字段是 `name`**（不是 topic_name）。
实测搜"云服务器"，候选依次为：`腾讯云服务器` / `华为云服务器` / `三丰云服务器` / `免费云服务器`。
直接取第一条会挂上竞品话题（踩平台红线、且像软文）。`zhihu.rs` 的打分：
完全相等 > 前缀 > 包含；凡名字含其它云品牌（阿里/腾讯/华为/京东/…）一律排除。
故默认话题用精确存在的 `免费云服务器` + `虚拟主机`。

## 与截图/厂商提交的关系

发布成功后拿到 `https://zhuanlan.zhihu.com/p/{id}`，交给 `src/screenshot.rs` 用 headless
Chrome 截全页图（知乎文章页公开可访问，一般无需过挑战），再把截图 + 文章 URL 一起
multipart 提交给云厂商的续期接口。

## 风险

从数据中心 IP 用个人知乎 Cookie 自动化发帖，画像异常，可能触发风控/封禁。代码对
401/403/验证码**立即停止且不重试**（反复撞风控才会真封号）。生产使用务必知情。
