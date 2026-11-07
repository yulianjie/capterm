//! TCP port forwarding on the authenticated target transport.

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use russh::{Channel, ChannelOpenFailure, client};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
    time::timeout,
};

use super::HostVerifier;

const MAX_STREAMS: usize = 256;
const SOCKS_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PortForward {
    Local {
        listen: ForwardEndpoint,
        target: ForwardEndpoint,
    },
    Remote {
        listen: ForwardEndpoint,
        target: ForwardEndpoint,
    },
    Dynamic {
        listen: ForwardEndpoint,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForwardEndpoint {
    host: String,
    port: u16,
}

impl PortForward {
    #[cfg(test)]
    pub(super) fn listen_port(&self) -> u16 {
        match self {
            Self::Local { listen, .. } | Self::Remote { listen, .. } | Self::Dynamic { listen } => {
                listen.port
            }
        }
    }

    pub fn parse(keyword: &str, args: &[&str]) -> Result<Self> {
        let keyword = keyword.to_ascii_lowercase();
        Ok(match (keyword.as_str(), args) {
            ("localforward", [listen, target]) => Self::Local {
                listen: ForwardEndpoint::parse(listen, Some("127.0.0.1"))?,
                target: ForwardEndpoint::parse(target, None)?,
            },
            ("remoteforward", [listen, target]) => Self::Remote {
                listen: ForwardEndpoint::parse(listen, Some("127.0.0.1"))?,
                target: ForwardEndpoint::parse(target, None)?,
            },
            ("dynamicforward", [listen]) => Self::Dynamic {
                listen: ForwardEndpoint::parse(listen, Some("127.0.0.1"))?,
            },
            _ => bail!(
                "转发规则须使用 LocalForward/RemoteForward 监听端口 目标地址，或 DynamicForward 监听端口"
            ),
        })
    }

    pub fn parse_line(line: &str) -> Result<Self> {
        let (keyword, rest) = line
            .trim()
            .split_once(char::is_whitespace)
            .context("转发规则缺少地址")?;
        Self::parse(keyword, &rest.split_whitespace().collect::<Vec<_>>())
    }

    pub fn openssh_args(&self) -> (&'static str, String) {
        match self {
            Self::Local { listen, target } => ("-L", format!("{listen}:{target}")),
            Self::Remote { listen, target } => ("-R", format!("{listen}:{target}")),
            Self::Dynamic { listen } => ("-D", listen.to_string()),
        }
    }
}

impl fmt::Display for PortForward {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local { listen, target } => write!(f, "LocalForward {listen} {target}"),
            Self::Remote { listen, target } => write!(f, "RemoteForward {listen} {target}"),
            Self::Dynamic { listen } => write!(f, "DynamicForward {listen}"),
        }
    }
}

impl ForwardEndpoint {
    fn parse(value: &str, default_host: Option<&str>) -> Result<Self> {
        let (host, port) = if let Some(value) = value.strip_prefix('[') {
            let (host, port) = value
                .split_once("]:")
                .context("IPv6 地址须写为 [地址]:端口")?;
            ensure!(host.parse::<Ipv6Addr>().is_ok(), "无效 IPv6 地址：{host}");
            (host, port)
        } else if let Some((host, port)) = value.split_once(':') {
            (host, port)
        } else {
            (default_host.context("转发目标须包含主机与端口")?, value)
        };
        let port = port.parse::<u16>().context("无效的 TCP 端口")?;
        ensure!(default_host.is_some() || port != 0, "转发目标端口不能为 0");
        ensure!(
            !host.is_empty() || default_host.is_some(),
            "转发目标主机不能为空"
        );
        ensure!(
            default_host.is_some() || host != "*",
            "转发目标须为单个主机"
        );
        ensure!(
            !host
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '[' | ']' | '/')),
            "无效的转发主机：{host}"
        );
        Ok(Self {
            host: if host.is_empty() {
                "*".into()
            } else {
                host.to_owned()
            },
            port,
        })
    }

    fn local_bind_host(&self) -> &str {
        if self.host == "*" {
            "0.0.0.0"
        } else {
            &self.host
        }
    }

    fn remote_bind_host(&self) -> &str {
        if self.host == "*" { "" } else { &self.host }
    }
}

impl fmt::Display for ForwardEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[derive(Default)]
pub(super) struct RemoteForwards(Mutex<RemoteState>);

#[derive(Default)]
struct RemoteState {
    accepting: bool,
    destinations: Vec<(ForwardEndpoint, ForwardEndpoint)>,
    streams: JoinSet<()>,
}

impl RemoteForwards {
    fn register(&self, listen: ForwardEndpoint, target: ForwardEndpoint) {
        self.0.lock().unwrap().destinations.push((listen, target));
    }

    pub(super) async fn handle(
        &self,
        channel: Channel<client::Msg>,
        host: &str,
        port: u32,
        reply: client::ChannelOpenHandle,
        connect_timeout: Duration,
    ) {
        let reply = {
            let mut state = self.0.lock().unwrap();
            while state.streams.try_join_next().is_some() {}
            let destination = (state.accepting && state.streams.len() < MAX_STREAMS)
                .then(|| find_remote_destination(&state.destinations, host, port))
                .flatten();
            if let Some(target) = destination {
                state.streams.spawn(async move {
                    let socket = timeout(
                        connect_timeout,
                        TcpStream::connect((target.host.as_str(), target.port)),
                    )
                    .await;
                    if let Ok(Ok(mut socket)) = socket {
                        reply.accept().await;
                        let mut channel = channel.into_stream();
                        let _ = io::copy_bidirectional(&mut socket, &mut channel).await;
                    } else {
                        reply.reject(ChannelOpenFailure::ConnectFailed).await;
                    }
                });
                return;
            }
            reply
        };
        reply
            .reject(ChannelOpenFailure::AdministrativelyProhibited)
            .await;
    }

    fn activate(&self) {
        self.0.lock().unwrap().accepting = true;
    }

    fn close(&self) {
        let mut state = self.0.lock().unwrap();
        state.accepting = false;
        state.destinations.clear();
        state.streams.abort_all();
    }
}

fn find_remote_destination(
    rules: &[(ForwardEndpoint, ForwardEndpoint)],
    host: &str,
    port: u32,
) -> Option<ForwardEndpoint> {
    let mut eligible = rules
        .iter()
        .filter(|(listen, _)| u32::from(listen.port) == port);
    let ip = host.parse::<IpAddr>().ok();
    eligible
        .clone()
        .find(|(listen, _)| listen.remote_bind_host().eq_ignore_ascii_case(host))
        .or_else(|| {
            eligible.find(|(listen, _)| match listen.host.as_str() {
                "*" => true,
                "0.0.0.0" => ip.is_some_and(|ip| ip.is_ipv4()),
                "::" => ip.is_some_and(|ip| ip.is_ipv6()),
                "localhost" => ip.is_some_and(|ip| ip.is_loopback()),
                address => {
                    host.eq_ignore_ascii_case("localhost")
                        && address.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
                }
            })
        })
        .map(|(_, target)| target.clone())
}

pub(super) struct Forwarding {
    remote: Arc<RemoteForwards>,
    listeners: Vec<JoinHandle<()>>,
    pub(super) active: Vec<PortForward>,
}

impl Forwarding {
    pub(super) async fn start(
        session: &Arc<client::Handle<HostVerifier>>,
        remote: Arc<RemoteForwards>,
        rules: &[PortForward],
        connect_timeout: Duration,
    ) -> Result<Self> {
        let mut forwarding = Self {
            remote,
            listeners: Vec::new(),
            active: Vec::new(),
        };
        let mut pending = Vec::new();
        for rule in rules {
            match rule {
                PortForward::Local { listen, target } => {
                    let (socket, listen) = bind(listen)
                        .await
                        .with_context(|| format!("本地转发无法监听 {listen}"))?;
                    pending.push((socket, Some(target.clone())));
                    forwarding.active.push(PortForward::Local {
                        listen,
                        target: target.clone(),
                    });
                }
                PortForward::Dynamic { listen } => {
                    let (socket, listen) = bind(listen)
                        .await
                        .with_context(|| format!("动态转发无法监听 {listen}"))?;
                    pending.push((socket, None));
                    forwarding.active.push(PortForward::Dynamic { listen });
                }
                PortForward::Remote { listen, target } => {
                    let allocated = timeout(
                        connect_timeout,
                        session.tcpip_forward(listen.remote_bind_host(), u32::from(listen.port)),
                    )
                    .await
                    .with_context(|| format!("远程转发请求超时：{listen}"))?
                    .with_context(|| format!("服务器未允许远程转发监听 {listen}"))?;
                    let mut listen = listen.clone();
                    if listen.port == 0 {
                        listen.port =
                            u16::try_from(allocated).context("服务器返回的转发端口无效")?;
                        ensure!(listen.port != 0, "服务器未返回动态分配的远程端口");
                    }
                    forwarding.remote.register(listen.clone(), target.clone());
                    forwarding.active.push(PortForward::Remote {
                        listen,
                        target: target.clone(),
                    });
                }
            }
        }
        forwarding.remote.activate();
        forwarding.listeners = pending
            .into_iter()
            .map(|(socket, target)| {
                tokio::spawn(accept_local(
                    socket,
                    Arc::clone(session),
                    target,
                    connect_timeout,
                ))
            })
            .collect();
        Ok(forwarding)
    }

    pub(super) async fn close(&self, session: &client::Handle<HostVerifier>) {
        self.stop();
        let _ = timeout(Duration::from_secs(2), async {
            for rule in &self.active {
                if let PortForward::Remote { listen, .. } = rule {
                    let _ = session
                        .cancel_tcpip_forward(listen.remote_bind_host(), u32::from(listen.port))
                        .await;
                }
            }
        })
        .await;
    }

    fn stop(&self) {
        for listener in &self.listeners {
            listener.abort();
        }
        self.remote.close();
    }
}

impl Drop for Forwarding {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn bind(listen: &ForwardEndpoint) -> Result<(TcpListener, ForwardEndpoint)> {
    let socket = TcpListener::bind((listen.local_bind_host(), listen.port)).await?;
    let port = socket.local_addr()?.port();
    Ok((
        socket,
        ForwardEndpoint {
            port,
            ..listen.clone()
        },
    ))
}

async fn accept_local(
    listener: TcpListener,
    session: Arc<client::Handle<HostVerifier>>,
    fixed_target: Option<ForwardEndpoint>,
    connect_timeout: Duration,
) {
    let mut streams = JoinSet::new();
    loop {
        tokio::select! {
            _ = streams.join_next(), if !streams.is_empty() => {}
            accepted = listener.accept() => {
                let Ok((socket, peer)) = accepted else { break };
                if streams.len() < MAX_STREAMS {
                    let session = Arc::clone(&session);
                    let target = fixed_target.clone();
                    streams.spawn(async move {
                        let _ = forward_local(socket, peer, session, target, connect_timeout).await;
                    });
                }
            }
        }
    }
}

async fn forward_local(
    mut socket: TcpStream,
    peer: SocketAddr,
    session: Arc<client::Handle<HostVerifier>>,
    fixed_target: Option<ForwardEndpoint>,
    connect_timeout: Duration,
) -> Result<()> {
    let socks = fixed_target.is_none();
    let target = match fixed_target {
        Some(target) => target,
        None => match timeout(SOCKS_TIMEOUT, socks_target(&mut socket)).await? {
            Ok(target) => target,
            Err(SocksError::Rejected(code)) => {
                return socks_reply(&mut socket, code)
                    .await
                    .context("回复 SOCKS5 请求");
            }
            Err(SocksError::Io(error)) => return Err(error.into()),
        },
    };
    let channel = timeout(
        connect_timeout,
        session.channel_open_direct_tcpip(
            target.host,
            u32::from(target.port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        ),
    )
    .await;
    let mut channel = match channel {
        Ok(Ok(channel)) => channel.into_stream(),
        Ok(Err(error)) => {
            if socks {
                let code = match error {
                    russh::Error::ChannelOpenFailure(
                        ChannelOpenFailure::AdministrativelyProhibited,
                    ) => 0x02,
                    russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed) => 0x05,
                    _ => 0x01,
                };
                socks_reply(&mut socket, code).await?;
            }
            return Err(error.into());
        }
        Err(error) => {
            if socks {
                socks_reply(&mut socket, 0x04).await?;
            }
            return Err(error.into());
        }
    };
    if socks {
        socks_reply(&mut socket, 0).await?;
    }
    io::copy_bidirectional(&mut socket, &mut channel).await?;
    Ok(())
}

#[derive(Debug)]
enum SocksError {
    Io(io::Error),
    Rejected(u8),
}

impl From<io::Error> for SocksError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

async fn socks_target(
    socket: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> Result<ForwardEndpoint, SocksError> {
    let mut greeting = [0; 2];
    socket.read_exact(&mut greeting).await?;
    if greeting[0] != 5 {
        return Err(io::Error::other("仅支持 SOCKS5").into());
    }
    let mut methods = vec![0; usize::from(greeting[1])];
    socket.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        socket.write_all(&[5, 0xff]).await?;
        return Err(io::Error::other("SOCKS5 客户端未提供无认证模式").into());
    }
    socket.write_all(&[5, 0]).await?;
    let mut request = [0; 4];
    socket.read_exact(&mut request).await?;
    if request[0] != 5 || request[2] != 0 {
        return Err(SocksError::Rejected(0x01));
    }
    if request[1] != 1 {
        return Err(SocksError::Rejected(0x07));
    }
    let host = match request[3] {
        1 => {
            let mut bytes = [0; 4];
            socket.read_exact(&mut bytes).await?;
            Ipv4Addr::from(bytes).to_string()
        }
        3 => {
            let mut bytes = vec![0; usize::from(socket.read_u8().await?)];
            socket.read_exact(&mut bytes).await?;
            let host = String::from_utf8(bytes).map_err(|_| SocksError::Rejected(0x08))?;
            if host.is_empty() || host.chars().any(|c| c.is_control() || c.is_whitespace()) {
                return Err(SocksError::Rejected(0x08));
            }
            host
        }
        4 => {
            let mut bytes = [0; 16];
            socket.read_exact(&mut bytes).await?;
            Ipv6Addr::from(bytes).to_string()
        }
        _ => return Err(SocksError::Rejected(0x08)),
    };
    let port = socket.read_u16().await?;
    if port == 0 {
        return Err(SocksError::Rejected(0x01));
    }
    Ok(ForwardEndpoint { host, port })
}

async fn socks_reply(socket: &mut (impl AsyncWrite + Unpin), code: u8) -> io::Result<()> {
    // The SSH channel does not expose the remote TCP socket's local endpoint.
    socket.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await
}
