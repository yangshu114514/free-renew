# 阿贝云免费服务器延期协议

> 2026-09 真账号实测验证。端点结构来自 [BookerLiu/FreeServer](https://github.com/BookerLiu/FreeServer)（2020），本项目复测确认存活。

## 端点

| 端点 | 用途 |
|---|---|
| `https://api.abeiyun.com/www/login.php` | 登录 |
| `https://api.abeiyun.com/www/renew.php` | 延期状态/记录/提交 |

## ⚠️ 境外 IP 注意

`api.abeiyun.com` 解析出一个假 AAAA（`fc00::6`，私有地址）+ A 记录 `198.20.0.6`。
境外 IP 直连 HTTPS 会被 WAF 拦（403 或超时）；**HTTP 80 端口实测正常**。
本工具在 `allow_http_fallback = true`（阿贝云默认开启）时自动尝试 https → http 降级。

## 接口明细

与三丰云同构（`cmd=` 表单协议），差异点如下：

### 登录

```
POST /www/login.php        # 境外走 http://
cmd=login&id_mobile={手机号}&password={密码}
```

成功：`{"response":"200","url":"/control","msg":"登录成功"}`
Cookie：`session_id`（HttpOnly，域为阿贝云；与三丰云同款机制）+ WAF Cookie（`acw_tc`/`cdn_sec_tc`）

### 查询延期状态

```
POST /www/renew.php
cmd=check_free_delay&ptype=vps
```

实测形状（2026-09，真账号）——**与三丰云不同，别混用解析器**：

```json
{"msg":{"delay_enable":0,"next_time":"2026-09-10 23:49:12"},"check":"e","response":"200"}
```

- `delay_enable` 是 **JSON 数字**（非字符串）
- 未到期时仅 `delay_enable:0` + `next_time`，无 `delay_state`

### 延期记录列表

```
POST /www/renew.php
cmd=free_delay_list&ptype=vps&count=20&page=1
```

结构同三丰云（`msg.content[]`，`State` 同款中文状态字）。

### 提交延期

```
POST /www/renew.php
multipart: cmd=free_delay_add, ptype=vps, url={文章URL}, yanqi_img={截图}
```

与三丰云一致。

## 续期周期

免费服务器有效期 5 天，到期前可续（实测 `next_time` 给出精确到秒的可续时刻）。
官方规则（content_1155）：在论坛/社区发体验帖 + 截图提交审核，可无限次续期。

