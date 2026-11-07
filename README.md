# CapTerm

CapTerm 是用 Rust 和 [GPUI Kit](https://github.com/longbridge/gpui-kit) 构建的 SSH 工作台。左侧是 SFTP 文件管理，中间是多标签终端和命令输入栏，右侧是保存的会话和主机监控。深色主题采用石墨黑，浅色主题采用暖白，界面以灰紫色标记当前操作。

[打开交互设计稿](docs/design/workbench.html)。设计稿包含示例数据，可以切换主题、打开文件编辑器，并通过缩放浏览器窗口查看折叠侧栏。

## 构建和运行

Windows 使用 Rust 1.95 或更高版本的 MSVC 工具链、Visual Studio C++ Build Tools 和 Windows SDK。

```powershell
cargo run --locked
cargo test --locked
```

## 使用

- **会话** · 通过 **New session** 保存连接信息，通过右侧的测试按钮验证 SSH 握手与认证。右键菜单提供编辑、删除和密码连接。首次连接显示主机密钥指纹，已信任的主机密钥发生变化时，连接会中止。
- **跳板和导入** · 跳板可填写 `user@host:port`，多跳用逗号分隔。**Import SSH config** 支持自选路径和重新导入，涵盖 `Host`、`HostName`、`User`、`Port`、`IdentityFile`、`ProxyJump` 和 `Include`。导入使用配置文件的静态子集。条件 `Match` 和可执行指令会跳过或提示，依赖 `ProxyCommand` 的会话需要改为 `ProxyJump`。
- **终端** · 点击终端即可直接操作远程 PTY。全屏程序进入备用屏幕时自动收起行号和时间，也可以用 **TUI** 手动切换。**# Time** 控制行号与时间列。行号对应保留的物理行，时间按本地时区显示该行最近一次可识别的输出更新。滚动历史淘汰或调整窗口重排时，行号可能变化，无法归属的时间留空。
- **本地 Shell** · Windows 上点击 **Local shell** 直接打开 PowerShell，旁边的菜单可以选择 Command Prompt（cmd）或 WSL。WSL 使用系统配置的默认发行版。本地 Shell 各自使用独立的终端标签。
- **SFTP** · 点击目录进入，点击 UTF-8 文本文件打开编辑器。覆盖保存需要服务端支持 OpenSSH 的原子重命名扩展，并会检查编辑期间远端文件是否已变化。将本地文件拖到左侧上传。在 Windows 上将远程文件从左侧列表直接拖到资源管理器文件夹，松开鼠标即可下载，也可以使用右键下载。
- **监控** · 右侧底部显示当前会话的 CPU、内存、磁盘和 SSH 往返延迟。资源采样针对 Linux 主机，延迟来自 SSH 协议探测。窄窗口通过顶部 **Files** 和 **Sessions** 按钮打开侧栏。

密码仅在连接目标主机时使用。跳板使用私钥或 SSH agent。加密私钥可先加载到本机 SSH agent，再由 CapTerm 使用。应用使用独立的 `known_hosts` 保存已确认的主机密钥。
