# 三丰云免费服务器延期协议

> 2026-09 真账号实测验证。协议原始出处 [BookerLiu/FreeServer](https://github.com/BookerLiu/FreeServer)（2020 逆向），本项目复测确认存活。

## 端点

| 端点 | 用途 |
|---|---|
| `https://api.sanfengyun.com/www/login.php` | 登录 |
| `https://api.sanfengyun.com/www/renew.php` | 延期状态/记录/提交 |

## 认证方式

官方控制台（Vue SPA）的 axios 配置（`app.js` E1BW 模块）：

```js
baseURL = "https://api.sanfengyun.com/"
withCredentials = true
// 请求拦截器只设置 Content-Type，无 token、无签名、无 CSRF
```

即：**登录获得 `session_id` Cookie（域 `.sanfengyun.com`，HttpOnly）后，一切操作就是普通表单 POST**。

## 接口明细

### 登录

```
POST /www/login.php
Content-Type: application/x-www-form-urlencoded

cmd=login&id_mobile={手机号}&password={密码}
```

成功：`{"response":"200","url":"\/control","msg":"登录成功"}`
失败：`{"response":"500101","msg":"登陆失败，密码错误!"}`
未登录调任何接口：`{"response":"50140","msg":"您尚未登陆！"}`（未知 cmd 也返回这个）

### 查询延期状态

```
POST /www/renew.php
cmd=check_free_delay&ptype=vps
```

实测形状（2026-09，真账号）：

- 提交后待审核：`{"msg":{"delay_state":"审核中"},"response":"200"}` —— **中文状态字**
- 未到期：`delay_enable`（字符串 "0"/"1"）+ `next_time`

### 延期记录列表（只读）

```
POST /www/renew.php
cmd=free_delay_list&ptype=vps&count=20&page=1
```

返回 `msg.content[]`：每条含 `id/url/img_fatie(截图URL)/SetTime/CheckTime/State/CheckState`，
`State` 取值实测：`审核通过` / `审核中` / `内容不存在或已被删除`。

### 提交延期（核心）

```
POST /www/renew.php
Content-Type: multipart/form-data（boundary 由 HTTP 库生成）

cmd=free_delay_add
ptype=vps
url={文章URL}
yanqi_img={文章页截图, image/png}
```

成功：`提交成功`。审核为人工，通常工作时间内数小时出结果。

## 审核红线（官方帮助文档 content_1156）

- 必含「三丰云」「免费虚拟主机」「免费云服务器」+ ≥50 字使用感受
- 含官网链接 / 图文并茂 → 优先通过
- **禁止出现「申请延期」字样**
- **自建博客站（含 GitHub Pages）因无展现量不通过**；重复内容不再通过
- 提交时文章发布时间超过 1 小时 → 失败（本工具流程天然满足时序）

## ptype 说明

`vps` = 免费云服务器；`vhost` = 免费虚拟主机（另一套产品，字段结构类似）。
