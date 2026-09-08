# CSDN 发文协议（Markdown 编辑器）

> 2026-09 真账号实测发文成功。签名算法出处为 CSDN 前端 JS（app.chunk.*.js），社区流传的 Java 复现版有一处关键错误，本文档以 JS 原版为准。

## 端点

```
POST https://bizapi.csdn.net/blog-console-api/v3/mdeditor/saveArticle
Content-Type: application/json
```

## 认证

- **Cookie**：登录态（关键字段 `UserName` / `UserToken` / `UN` / `p_uid`）。
  本仓库附带采集器 `csdn_cookie_export`（有头 Chrome 扫码 → 自动导出单行 Cookie）。
- **x-ca 签名四件套**：

| Header | 值 |
|---|---|
| `x-ca-key` | `203803574`（前端内嵌常量） |
| `x-ca-nonce` | 随机 UUID v4 |
| `x-ca-signature` | 见下 |
| `x-ca-signature-headers` | `x-ca-key,x-ca-nonce` |

appSecret（前端内嵌公开常量）：`9znpamsyl2c7cdrr9sas0le9vbc3r6ba`

## 签名算法（HMAC-SHA256 → Base64）

### ⚠️ 待签串格式（最大的坑）

社区流传的 Java 版把待签串写成：

```
POST\n*/*\napplication/json\n\nx-ca-key:...     ← 错！
```

**JS 原版（实测验证正确）是 accept 与 content-type 之间有一个空行**：

```
POST\n*/*\n\napplication/json\n\nx-ca-key:203803574\nx-ca-nonce:{uuid}\n/blog-console-api/v3/mdeditor/saveArticle
```

即完整结构：

```
{method}\n
{accept}\n
\n            ← 空行
{content-type}\n
\n            ← 空行
x-ca-key:{key}\n
x-ca-nonce:{nonce}\n
{path}        ← 无查询串；带 query 时 path 后拼 ?k=v&...（参数需排序）
```

错误表现为服务端返回 `{"message":"HMAC signature does not match"}`。
`src/csdn.rs` 的单元测试锁定了正确形状（base64 长度恒 44）。

## 请求体

```json
{
  "title": "标题",
  "content": "<p>HTML 正文</p>",
  "markdowncontent": "markdown 正文",
  "pubStatus": "publish",          // 或 "draft"
  "readType": "public",
  "type": "original",
  "tags": "tag1,tag2",             // 逗号分隔，最多 5 个
  "categories": "",
  "creation_statement": 1,         // 0=无声明 1=AI辅助 2=整合 3=个人观点
  "status": 0,                     // 0=发布 2=草稿
  "cover_type": 1,
  "authorized_status": false,
  "source": "pc_mdeditor"
}
```

## 成功响应

```json
{
  "code": 200,
  "data": {
    "url": "https://blog.csdn.net/{用户名}/article/details/{id}",
    "id": 164634232,
    "title": "...",
    "description": ""
  },
  "msg": "success"
}
```

`data.url` 即审核员看到的最终文章页。

## 已知坑

1. **中文请求体**：用 shell（curl/PowerShell）手测时 GBK/UTF-8 编码问题会导致服务端报「博主不存在」（实际是乱码 body 触发的 400）。程序内构造 JSON 无此问题。
2. **频率限制**：连续发布会触发「文章频繁发布，请稍后再试」。
3. **必须先开通博客**：新注册账号直接调接口会报「博主不存在」——先在 blog.csdn.net 完成开通流程。
