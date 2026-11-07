use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot},
    time::timeout,
};

use super::{ConnectionPrompt, ProxyCommandRequest, TRUST_TIMEOUT, normalize_host};
use crate::config::{self, NetworkProxy, ProxyKind};

pub(super) struct ProxyProcess {
    stream: io::Join<ChildStdout, ChildStdin>,
    _process: Child,
}

impl AsyncRead for ProxyProcess {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buffer)
    }
}

impl AsyncWrite for ProxyProcess {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, data)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

pub(super) async fn run_command(
    template: &[String],
    host: &str,
    port: u16,
    user: &str,
    alias: &str,
    prompts: &mpsc::UnboundedSender<ConnectionPrompt>,
) -> Result<ProxyProcess> {
    let mut argv = config::expand_proxy_command(template, host, port, user, alias)?;
    let program = argv.first().context("ProxyCommand 需要可执行文件")?;
    let executable = resolve_executable(program)?;
    argv[0] = config::display_path(&executable.to_string_lossy());
    let displayed = config::format_argv(&argv);
    let (reply, answer) = oneshot::channel();
    prompts
        .send(ConnectionPrompt::ProxyCommand(ProxyCommandRequest {
            destination: config::ssh_address(user, host, Some(port)),
            command: displayed,
            reply,
        }))
        .map_err(|_| anyhow::anyhow!("无法确认 ProxyCommand"))?;
    ensure!(
        timeout(TRUST_TIMEOUT, answer)
            .await
            .context("等待确认 ProxyCommand 超时")?
            .unwrap_or(false),
        "已取消 ProxyCommand"
    );
    let mut command = Command::new(executable);
    command
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut process = command.spawn().context("启动 ProxyCommand 失败")?;
    let input = process.stdin.take().context("ProxyCommand 没有标准输入")?;
    let output = process.stdout.take().context("ProxyCommand 没有标准输出")?;
    Ok(ProxyProcess {
        stream: io::join(output, input),
        _process: process,
    })
}

fn resolve_executable(program: &str) -> Result<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() || path.components().count() != 1 || program.contains(['/', '\\']) {
        return executable_file(path).with_context(|| format!("找不到可执行文件 {program}"));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            if let Some(executable) = executable_file(&directory.join(path)) {
                return Ok(executable);
            }
        }
    }
    bail!("PATH 中找不到可执行文件 {program}")
}

fn executable_file(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let candidate = match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("exe") => path.to_owned(),
        // An explicit .bat or .cmd would introduce an implicit command shell.
        Some(_) => return None,
        None => path.with_extension("exe"),
    };
    #[cfg(not(windows))]
    let candidate = path.to_owned();
    let metadata = candidate.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    let canonical = candidate.canonicalize().ok()?;
    #[cfg(windows)]
    if !canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return None;
    }
    Some(canonical)
}

pub(super) async fn connect_network(
    proxy: &NetworkProxy,
    host: &str,
    port: u16,
    limit: Duration,
) -> Result<TcpStream> {
    let proxy_host = normalize_host(&proxy.host)?;
    timeout(limit, async {
        let mut stream = TcpStream::connect((proxy_host.as_str(), proxy.port))
            .await
            .context("无法连接代理服务器")?;
        match proxy.kind {
            ProxyKind::Socks5 => socks5(&mut stream, host, port).await?,
            ProxyKind::HttpConnect => http_connect(&mut stream, host, port).await?,
        }
        Ok(stream)
    })
    .await
    .context("连接代理服务器超时")?
}

async fn socks5(stream: &mut TcpStream, host: &str, port: u16) -> Result<()> {
    stream.write_all(&[5, 1, 0]).await?;
    let mut hello = [0u8; 2];
    stream.read_exact(&mut hello).await?;
    ensure!(hello == [5, 0], "SOCKS5 代理未接受无认证方式");
    let mut request = vec![5, 1, 0];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(1);
            request.extend(ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(4);
            request.extend(ip.octets());
        }
        Err(_) => {
            request.push(3);
            request.push(u8::try_from(host.len()).context("SOCKS5 目标主机名过长")?);
            request.extend(host.as_bytes());
        }
    }
    request.extend(port.to_be_bytes());
    stream.write_all(&request).await?;
    let mut response = [0u8; 4];
    stream.read_exact(&mut response).await?;
    ensure!(response[0] == 5 && response[2] == 0, "SOCKS5 响应格式无效");
    ensure!(
        response[1] == 0,
        "SOCKS5 代理拒绝连接（状态 {}）",
        response[1]
    );
    let length = match response[3] {
        1 => 4,
        4 => 16,
        3 => stream.read_u8().await? as usize,
        _ => bail!("SOCKS5 响应地址类型无效"),
    };
    let mut address = [0u8; 257];
    stream.read_exact(&mut address[..length + 2]).await?;
    Ok(())
}

async fn http_connect(stream: &mut TcpStream, host: &str, port: u16) -> Result<()> {
    ensure!(
        host.bytes()
            .all(|byte| byte.is_ascii_graphic() && !b"/@[]".contains(&byte)),
        "HTTP CONNECT 目标主机名无效"
    );
    let address = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    stream
        .write_all(format!("CONNECT {address} HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
        .await?;
    let mut headers = Vec::with_capacity(256);
    while !headers.ends_with(b"\r\n\r\n") {
        ensure!(headers.len() < 16 * 1024, "HTTP CONNECT 响应头过长");
        headers.push(stream.read_u8().await?);
    }
    let status = headers
        .split(|&byte| byte == b'\n')
        .next()
        .unwrap_or_default();
    let status = std::str::from_utf8(status).context("HTTP CONNECT 状态行无效")?;
    let mut parts = status.split_ascii_whitespace();
    let version = parts.next().unwrap_or_default();
    let code = parts
        .next()
        .unwrap_or_default()
        .parse::<u16>()
        .context("HTTP CONNECT 状态码无效")?;
    ensure!(version.starts_with("HTTP/1."), "HTTP CONNECT 响应协议无效");
    ensure!(
        (200..300).contains(&code),
        "HTTP CONNECT 代理拒绝连接（状态 {code}）"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn socks5_preserves_tunneled_bytes() -> Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let proxy = NetworkProxy {
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".into(),
            port: listener.local_addr()?.port(),
        };
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut greeting = [0u8; 3];
            stream.read_exact(&mut greeting).await?;
            assert_eq!(greeting, [5, 1, 0]);
            stream.write_all(&[5, 0]).await?;
            let mut request = [0u8; 18];
            stream.read_exact(&mut request).await?;
            assert_eq!(&request[..5], &[5, 1, 0, 3, 11]);
            assert_eq!(&request[5..16], b"example.com");
            assert_eq!(&request[16..], &2222u16.to_be_bytes());
            stream
                .write_all(b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x16SSH")
                .await?;
            Ok::<_, anyhow::Error>(())
        });
        let mut stream =
            connect_network(&proxy, "example.com", 2222, Duration::from_secs(3)).await?;
        let mut payload = [0u8; 3];
        stream.read_exact(&mut payload).await?;
        assert_eq!(&payload, b"SSH");
        peer.await??;
        Ok(())
    }

    #[tokio::test]
    async fn http_connect_preserves_tunneled_bytes_and_ipv6_authority() -> Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let proxy = NetworkProxy {
            kind: ProxyKind::HttpConnect,
            host: "127.0.0.1".into(),
            port: listener.local_addr()?.port(),
        };
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let expected = b"CONNECT [::1]:22 HTTP/1.1\r\nHost: [::1]:22\r\n\r\n";
            let mut request = vec![0u8; expected.len()];
            stream.read_exact(&mut request).await?;
            assert_eq!(request, expected);
            stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\nSSH")
                .await?;
            Ok::<_, anyhow::Error>(())
        });
        let mut stream = connect_network(&proxy, "::1", 22, Duration::from_secs(3)).await?;
        let mut payload = [0u8; 3];
        stream.read_exact(&mut payload).await?;
        assert_eq!(&payload, b"SSH");
        peer.await??;
        Ok(())
    }

    #[tokio::test]
    async fn proxy_command_waits_for_explicit_approval() -> Result<()> {
        let (prompts, mut pending) = mpsc::unbounded_channel();
        let program = std::env::current_exe()?.to_string_lossy().into_owned();
        let template = [program, "--hostname".into(), "%h:%p".into()];
        let decline = async move {
            let ConnectionPrompt::ProxyCommand(request) =
                pending.recv().await.expect("approval request")
            else {
                panic!("expected ProxyCommand confirmation");
            };
            assert_eq!(request.destination, "ops@example.com:2222");
            assert!(request.command.contains("--hostname example.com:2222"));
            request.reply.send(false).unwrap();
        };
        let (result, ()) = tokio::join!(
            run_command(&template, "example.com", 2222, "ops", "alias", &prompts),
            decline
        );
        assert!(result.is_err());
        Ok(())
    }
}
