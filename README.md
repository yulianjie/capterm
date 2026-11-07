# CapTerm

CapTerm 是用 Rust 和 [GPUI Kit](https://github.com/longbridge/gpui-kit) 构建的 SSH 工作台。左侧管理 SFTP 文件与传输，中间运行终端和远程编辑器，右侧管理会话并监控当前主机。夜航和白昼主题共用同一布局，用品红标记 SSH 航线、用青色标记交互。

[设计说明、交互原型和导出画板](design/README.md) · [Linux 终端输出高亮规则](docs/linux-highlights.md)

## 下载

从 [GitHub Releases](https://github.com/yulianjie/capterm/releases) 下载 Windows x64 版本。运行 `*-setup.exe` 可为当前用户安装并创建开始菜单快捷方式。也可以解压 `*-portable.zip` 后直接运行 `capterm.exe`。同页提供 `SHA256SUMS.txt`，可用 `Get-FileHash -Algorithm SHA256` 核对下载文件。

## 构建和运行

Windows x64 使用 Rust 1.95 或更高版本的 MSVC 工具链、Visual Studio C++ Build Tools 和 Windows SDK。

```powershell
cargo run --locked
cargo test --locked
```

要在本地打包，先安装 Inno Setup 6.3 或以上的 6.x 版本，再用 PowerShell 7 运行以下命令。将 `-Version` 的值设为 `Cargo.toml` 中的版本号，产物位于 `dist/`。

```powershell
cargo build --release --locked
./scripts/package-windows.ps1 -Version 0.1.0 -Executable target/release/capterm.exe
```

## 自动出包

[Windows build and release](https://github.com/yulianjie/capterm/actions/workflows/windows.yml) 会在推送到 `main`、提交面向 `main` 的 PR 或手动运行时检查格式、运行测试并生成安装版和便携版。通过后可以从对应运行的 **Artifacts** 下载。

发布新版本时，更新 `Cargo.toml` 中的版本号和 `Cargo.lock`，将提交推送到 `main`，再推送同版本的 `vX.Y.Z` 标签。工作流验证标签与项目版本一致后，自动将安装版、便携版和校验文件发布到 **Releases**。

```powershell
git tag -a v0.1.0 -m "CapTerm v0.1.0"
git push origin v0.1.0
```

## 使用

- **会话与跳板**。双击保存的会话即可连接，也可按 `Ctrl+Shift+P` 输入 `user@host[:port]` 快速连接。航线逐跳显示握手和认证状态。会话支持跳板排序与复用、连接测试、网络代理以及本地、远程和动态 TCP 转发。
- **SSH 配置**。选择配置文件后逐项预览，再链接并同步或复制为本地会话。预览会标明跳过的配置和不适用的选项。本地会话可以导出 OpenSSH 配置片段。首次遇到主机密钥时，可仅本次信任或保存密钥。替换已有密钥需要输入对应主机名。
- **终端与发送栏**。直接操作远程 PTY，或在发送栏编辑后逐行执行、整段粘贴。已保存会话的发送历史随本机设置保存。终端显示逻辑行号、写入时间和命令块，备用屏幕改用屏幕行号。按 `Ctrl+Shift+F` 搜索输出。Windows 仍可通过新标签菜单打开 PowerShell、cmd 或 WSL。
- **远程文件**。单击文本文件打开预览，开始编辑后固定标签。保存前检查远端版本，可比较差异或重新加载。覆盖保存需要服务端支持 OpenSSH 的原子重命名扩展。上传与下载在传输队列显示进度和冲突，Windows 支持把远程文件拖到资源管理器。
- **监控与布局**。监控通过现有 SSH 连接采集目标主机的 CPU、内存、磁盘、网络、负载和进程。宽度低于 1200 px 时右侧缩为图标栏，低于 900 px 时左侧也收起。按 `Ctrl+Shift+Enter` 切换专注模式。

应用使用独立的 `known_hosts`。密码用于目标主机认证，跳板使用私钥或 SSH Agent。可将加密私钥先加载到本机 SSH Agent。
