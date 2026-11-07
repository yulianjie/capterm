# Linux 终端高亮规则

内置规则位于 [`assets/terminal/linux-highlights.json`](../assets/terminal/linux-highlights.json)。匹配类别参考 [WindTerm 2.7.0 发布包](https://github.com/kingToolbox/WindTerm/releases/tag/2.7.0)中的 Linux 高亮，配置使用 CapTerm 自己的 JSON 格式。

## 覆盖内容

- 提示符末尾的 `$`、`#`、`%`、`>`、`❯` 和 `` 标记，以及命令名、管道、选项、赋值、变量、运算符和括号。
- 引号字符串、常见转义与 printf 占位符。
- 输出中的错误、成功、警告、调试和信息词，以及 `ls -l` 权限位中的文件类型、读、写和执行字符。
- URL、IPv4 / IPv6、MAC 地址、日期、时间、月份、星期、源码位置、路径、十六进制值和数字。

## 规则格式

每条规则包含 `scope`、Rust `regex` 语法的 `pattern`，以及把捕获组编号映射到颜色的 `captures`。组 `0` 表示整段匹配，其他编号对应正则捕获组。同一规则内，较高编号的捕获组优先；规则之间仍按文件顺序逐字符处理，首次命中才设置颜色。

| 字段 | 含义 |
| --- | --- |
| `prompt` | 已识别的提示符 |
| `command` | 提示符之后输入的命令 |
| `output` | 终端输出 |
| `both` | 命令输入和终端输出 |

颜色可用 `black`、`red`、`green`、`yellow`、`blue`、`magenta`、`cyan`、`white`，对应的 `bright_*` 名称，或 `#RRGGBB`。未识别为提示符的逻辑行会回退到 `output` 范围；逻辑行超过 4096 字节时跳过这些规则。范围或模式之外的字符保持默认前景色。

高亮仅应用于原本使用默认前景色的字符。终端 ANSI 颜色始终优先；应用自带的绿色身份文字和蓝色路径文字保留其样式；全屏程序与 raw 模式不使用这些规则。

## 自定义

在 **设置 → 终端 → Linux 文本高亮** 中可启用或关闭高亮，SSH 与 WSL 共用此开关。选择 **编辑规则** 会在用户配置目录创建或打开 `linux-highlights.json`，保存后选择 **重载规则** 应用修改。规则文件的路径显示在同一设置区域。

用户文件会整体替换内置规则集，因此新增规则时应保留需要的其他规则。下面是一个最小配置示例：

```json
{
  "rules": [
    {
      "scope": "output",
      "pattern": "(?i)\\b(error|fatal)\\b",
      "captures": { "1": "bright_red" }
    }
  ]
}
```

每个配置最多 128 KiB、128 条规则；每个正则最多 4096 字节。初次读取无效文件时使用内置规则，重载无效文件时保留最近一次有效配置。
