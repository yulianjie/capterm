# CapTerm

[简体中文](README.md) · **English**

CapTerm is a native SSH workbench built with Rust and [GPUI Kit](https://github.com/longbridge/gpui-kit). Use the terminal, browse SFTP files, edit remote text, and monitor the host in one window. On Windows, PowerShell, Command Prompt, and WSL can run in neighboring tabs.

[Download CapTerm](https://github.com/yulianjie/capterm/releases/latest) · [Quick start](#quick-start) · [Build and release](#build-and-release)

![CapTerm in dark mode with SFTP files on the left, an SSH terminal in the center, and sessions and host monitoring on the right](docs/screenshots/ssh-workbench.jpg)

## Features

- **SSH sessions and jump hosts.** Save, group, and search sessions, or connect directly with `user@host[:port]`. The route shows handshake and authentication progress for each hop. Reuse multi-hop routes, test connections, configure network proxies, and set up local, remote, or dynamic TCP forwarding.
- **OpenSSH configuration.** Preview the import, then link the configuration file for future sync or copy its hosts as local sessions. The preview flags skipped entries and unsupported options. Export local sessions as OpenSSH configuration snippets.
- **Terminal and send bar.** Scrollback includes logical line numbers, timestamps, and command blocks. Search by text or regular expression, with optional case sensitivity. The send bar runs commands line by line or pastes the whole draft, with per-session history and reusable snippets. Full-screen programs such as vim and htop use the alternate-screen layout. The send bar defaults to whole-draft paste when it detects an agent CLI such as Codex or Claude Code.
- **SFTP and remote editing.** Let the file pane follow the terminal's current directory, drag files in to upload, or drag remote files out to Windows File Explorer. The transfer queue shows progress and conflicts. Click a remote text file to preview or edit it in a tab. CapTerm checks the remote version on save, and you can compare changes or reload when a conflict occurs. Overwriting a file from the editor requires the server's OpenSSH `posix-rename@openssh.com` extension.
- **Local files.** Browse Windows files from PowerShell and Command Prompt tabs, and Linux files from the WSL distribution, in the left file pane. The pane can follow the terminal directory, and Linux text files in WSL open in a read-only preview.
- **Host monitoring and layout.** View CPU, memory, disk, network, load, and process data through the current SSH connection without installing a collector on the server. Available metrics vary by remote operating system. The workbench has dark and light themes, collapsible sidebars, and a focus mode.

![CapTerm's light-theme remote editor with SFTP files on the left and process monitoring on the right](docs/screenshots/remote-editor.jpg)

The screenshots show CapTerm's Chinese interface running against an isolated local SSH demo host with sample terminal output, remote files, and metrics. See the [design notes and interactive prototype](design/README.md) for more scenarios. Customize SSH and WSL terminal text highlighting with the [Linux highlight rules](docs/linux-highlights.md).

## Download

Choose the package for your operating system and architecture on [GitHub Releases](https://github.com/yulianjie/capterm/releases/latest).

| System | File and installation |
| --- | --- |
| Windows x64 | Run `*-setup.exe`, or extract `*-portable.zip` and run `capterm.exe`. |
| macOS 15 or later | Choose `*-macos-arm64.dmg` for Apple Silicon or `*-macos-x86_64.dmg` for Intel. Open the disk image and drag CapTerm into Applications. |
| Ubuntu 24.04 | Choose `*-linux-amd64.deb` for x86-64 or `*-linux-arm64.deb` for ARM64. Install with `sudo apt install ./<filename>.deb`. CapTerm needs a desktop session with Vulkan support. |

Windows checksums are in `SHA256SUMS.txt`. macOS and Linux checksums are in `SHA256SUMS-unix.txt`. Use `Get-FileHash -Algorithm SHA256`, `shasum -a 256`, or `sha256sum` to verify downloads.

The macOS app uses an ad hoc signature and has no Apple notarization. On first launch, macOS may require Open Anyway under System Settings → Privacy & Security. Local PowerShell, Command Prompt, WSL, and dragging SFTP downloads out of the window are Windows features.

## Quick start

1. Press `Ctrl+Shift+P` to open the command palette and enter `user@host[:port]`. You can also double-click a saved session on the right or import OpenSSH configuration from the session pane.
2. On first connection, verify the host key fingerprint and choose whether to trust it for this connection or save it in CapTerm's own `known_hosts` file. To replace a saved key, enter the corresponding hostname.
3. Run commands in the terminal or open a remote text file from the SFTP pane on the left. After SSH disconnects, press `Enter` in the terminal or click the reconnect prompt to reconnect in the current tab. Press `Ctrl+Shift+F` to search terminal output and `Ctrl+Shift+Enter` to toggle focus mode.

Password authentication is available for the target host. Jump hosts use a private key or SSH agent. Load encrypted private keys into your local SSH agent.

## Build and release

Building on Windows x64 requires Rust 1.95 or later with the MSVC toolchain, Visual Studio C++ Build Tools, and the Windows SDK.
Building on macOS requires Rust 1.98 or later and the Xcode toolchain.

```powershell
cargo run --locked
cargo test --locked
```

Local packaging requires Inno Setup 6.3 or later in the 6.x series and PowerShell 7. Set `-Version` to the version in `Cargo.toml`. Packages are written to `dist/`.

```powershell
cargo build --release --locked
./scripts/package-windows.ps1 -Version 0.1.0 -Executable target/release/capterm.exe
```

The [Windows build workflow](https://github.com/yulianjie/capterm/actions/workflows/windows.yml) verifies the Windows build on pushes to `main`, pull requests targeting `main`, and manual dispatch. To release, update the version in `Cargo.toml` and `Cargo.lock`, push the commit to `main`, and push the matching `vX.Y.Z` tag. The [Desktop release workflow](https://github.com/yulianjie/capterm/actions/workflows/release.yml) checks the version and publishes packages and checksums after the Windows, macOS, and Linux builds pass. You can also dispatch it from `main` to add macOS and Linux packages to an existing release using its tag.

```powershell
git tag -a v0.1.0 -m "CapTerm v0.1.0"
git push origin v0.1.0
```

## License

CapTerm uses the [Apache License 2.0](LICENSE). The bundled [Microsoft ConPTY and OpenConsole](assets/windows/conpty/LICENSE) use the MIT license. [Maple Mono NF CN](assets/fonts/MapleMono-OFL.txt) and [Noto Sans SC](assets/fonts/NotoSansSC-OFL.txt) use the SIL Open Font License 1.1. See the [Lucide and Feather notices](assets/licenses/Lucide-Feather.txt) for icon licensing.
