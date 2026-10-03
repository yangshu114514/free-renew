#!/usr/bin/env bash
# 探针判据库（2026-10-03 新增）
#
# 为什么需要它：debug-probe.yml 里的探针原先全是**裸 curl** —— 无 -f、无状态码断言、
# 无 exit 1，于是无论返回什么 job 都 success。实测那 4 个 CSDN 探针从 10-02 首次运行
# 起**全部返回 HTTP 521**，workflow 却一直报 success。**假通过比失败更危险**：它制造
# "已验证"的错觉，比一次明确的失败糟糕得多。
#
# 判据总则：探针的价值在于"能把结果**明确归类**"，而不在于"必须 200"。
# 每一类探针都有自己"符合预期"的形态；真正要拦的是**无法归类**（000/超时/空体）——
# 那说明探针自身失效，而不是"环境正常"。

# normalize_code <raw>
#
# 把调用方给的 HTTP 码归一化成干净的 3 位数。
#
# 为什么必须做：本库第一版直接用 `[ "$code" = "000" ]` 判不可达，而调用方写的是
# `code=$(curl ... -w "%{http_code}" ... || echo "000")` —— curl 失败时**先自己输出
# 了 000**（非零退出），`||` 分支又补一个，于是拼成 `000000`，判据不匹配、
# 把"厂商完全连不上"报成了"✅ 均可达"。**这正是本库要根治的那类假通过，我自己先犯了一次。**
# 所以归一化放在库里，而不是指望每个调用方都写对。
normalize_code() {
    local c
    c=$(printf '%s' "${1:-}" | tr -cd '0-9' | cut -c1-3)
    [ -n "$c" ] || c="000"
    printf '%s' "$c"
}

# classify_csdn <http_code> <body_file> <label>
#
# CSDN 是**负向对照**：它对无 cookie 的裸请求固定下发 WAF 521 JS 挑战
# （响应体形如 window.onload=setTimeout("cr(22)",200) 的混淆 JS，2.1–2.4KB）；
# headless chrome 拿到的则是 ~371 字节的 "403 Forbidden" 错误页。
# 所以：
#   · 521 / 403 / 含 WAF 或 403 特征 → 符合预期（被挡住），通过
#   · 200 且含文章特征               → WAF 放行了。这是**基线变化**，值得知道，但不是失败
#   · 其余（000/超时/过小体/未知）    → 无法归类，失败
classify_csdn() {
    local code f label size=0
    code=$(normalize_code "$1"); f="$2"; label="$3"
    [ -f "$f" ] && size=$(wc -c < "$f" 2>/dev/null || echo 0)
    echo "  [$label] HTTP=$code size=$size"

    if [ "$code" = "000" ]; then
        echo "::error::$label 探针失效：根本连不上（HTTP=000）"
        return 1
    fi
    # ⚠️ 内容证据必须**先于**尺寸启发判断。真实的 403 错误页可以很小
    # （chrome --dump-dom 实测 371 字节；合成用例里甚至不到 100 字节），
    # 若先按尺寸拦，会把"被挡住"误判成"探针失效"——第一版正是这么误红的。
    # "被挡"的特征：WAF 的混淆 JS、或 403 错误页文本
    if [ "$code" = "521" ] || [ "$code" = "403" ] || \
       grep -qE 'cr\([0-9]+\)|setTimeout\("cr|window\.onload=setTimeout|403 Forbidden|Just a moment|Attention Required' "$f"; then
        echo "  → 归类：被 CSDN WAF 挡（负向对照符合预期；无 cookie 的裸请求本就拿不到文章）"
        return 0
    fi
    if [ "$code" = "200" ] && grep -qE 'article_content|article-title|article_title' "$f"; then
        echo "::notice::$label WAF 已放行且拿到文章——负向对照的基线变了（不是失败，但值得知道）"
        return 0
    fi
    if [ "$size" -lt 100 ]; then
        echo "::error::$label 探针失效：响应体仅 $size 字节且不含任何已知特征"
        return 1
    fi
    echo "::error::$label 无法归类：HTTP=$code 且响应体不含任何已知特征——探针或站点行为已变，需人工看"
    return 1
}

# classify_reachable <http_code> <label>
#
# 可达性探针：拿到**任何** HTTP 状态码都算"可达"（厂商端点回 4xx/5xx 也证明网络通），
# 只有 000（连不上/超时）才算失败。
# 这正是导致续期失败的故障模式——2026-10-03 实测：定时续期 17 次里 7 次挂在
# api.sanfengyun.com 连接超时；本探针修好归一化后立刻把它标红（HTTP=000000 → 000）。
classify_reachable() {
    local code
    code=$(normalize_code "$1")
    echo "  [$2] HTTP=$code"
    if [ "$code" = "000" ]; then
        echo "::error::$2 不可达（连接超时/DNS 失败）——这正是会让续期整轮失败的那类故障"
        return 1
    fi
    return 0
}
