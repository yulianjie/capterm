use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use russh::keys::{
    HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate,
    agent::{AgentIdentity, client::AgentClient},
    decode_secret_key,
    known_hosts::known_host_keys_path,
};
use russh::{Channel, ChannelMsg, Disconnect, client};
use russh_sftp::{
    client::{RawSftpSession, SftpSession},
    protocol::{FileAttributes, OpenFlags, Packet, StatusCode},
};
use tokio::{
    fs,
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{Mutex, OnceCell, mpsc, oneshot},
    time::timeout,
};

use crate::config::{ConnectionProfile, Hop, SshOptions};

mod agent_forward;
mod files;
mod forward;
mod jumps;
mod proxy;

use agent_forward::{AgentForwarding, AgentLease};
pub(crate) use files::{
    RemoteMutation, TransferChoice, TransferConflict, TransferOptions, TransferOutcome,
    TransferSignal,
};
pub use forward::PortForward;
use forward::{Forwarding, RemoteForwards};
use jumps::{JumpKey, SharedHop};

const NETWORK_TIMEOUT: Duration = Duration::from_secs(15);
const TRUST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PROBE_BYTES: usize = 64 * 1024;
const POSIX_RENAME: &str = "posix-rename@openssh.com";
static HOST_KEY_STORE: Mutex<()> = Mutex::const_new(());

pub struct TrustRequest {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub algorithm: String,
    pub known_hosts: PathBuf,
    pub previous_fingerprints: Vec<String>,
    pub route: Vec<String>,
    pub index: usize,
    pub reply: oneshot::Sender<TrustDecision>,
}

pub enum ConnectionPrompt {
    HostKey(TrustRequest),
    ProxyCommand(ProxyCommandRequest),
}

pub struct ProxyCommandRequest {
    pub destination: String,
    pub command: String,
    pub reply: oneshot::Sender<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustDecision {
    Reject,
    Once,
    Save,
}

#[derive(Clone, Debug)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub hidden: bool,
    pub size: Option<u64>,
    pub mtime: Option<u32>,
    pub permissions: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub cpu: Option<f64>,
    pub cores: Option<u64>,
    pub memory: Option<Usage>,
    pub swap: Option<Usage>,
    pub disks: Vec<DiskUsage>,
    pub load: Option<[f64; 3]>,
    pub uptime_secs: Option<u64>,
    pub rtt_ms: Option<u64>,
    pub hop_rtt_ms: Vec<Option<u64>>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub network_bps: Option<(f64, f64)>,
    pub processes: Vec<ProcessSample>,
    pub unsupported: bool,
    pub probe_available: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Usage {
    pub used: u64,
    pub total: u64,
    pub percent: f64,
}

#[derive(Clone, Debug)]
pub struct DiskUsage {
    pub mount: String,
    pub usage: Usage,
}

#[derive(Clone, Debug)]
pub struct ProcessSample {
    pub pid: u32,
    pub command: String,
    pub cpu: f64,
    pub memory: f64,
}

#[derive(Default)]
struct MetricCounters {
    cpu: Option<(u64, u64)>,
    network: Option<NetworkCounters>,
}

struct NetworkCounters {
    sampled_at: Instant,
    interfaces: HashMap<String, (u64, u64)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HopStage {
    Connecting,
    ProxyApproval,
    Trust,
    Authenticating,
    Connected,
    Failed,
}

#[derive(Clone, Debug)]
pub struct HopProgress {
    pub index: usize,
    pub stage: HopStage,
    pub rtt_ms: Option<u64>,
    pub authentication: Option<&'static str>,
    pub cipher: Option<String>,
    pub error: Option<String>,
}

pub enum ShellCommand {
    Input(Vec<u8>),
    Resize { columns: u16, rows: u16 },
    Close,
}

pub enum ShellEvent {
    Output(Vec<u8>),
    Exited(Option<u32>),
    Error(String),
}

struct HostVerifier {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    trust: mpsc::UnboundedSender<ConnectionPrompt>,
    route: Vec<String>,
    temporary_pin: Option<PublicKey>,
    persistent_key: Arc<OnceLock<PublicKey>>,
    awaiting_trust: Option<oneshot::Sender<()>>,
    index: usize,
    progress: Arc<dyn Fn(HopProgress) + Send + Sync>,
    cipher: Arc<OnceLock<String>>,
    agent_forward: Option<Arc<AgentForwarding>>,
    remote_forwards: Option<Arc<RemoteForwards>>,
    network_timeout: Duration,
}

impl client::Handler for HostVerifier {
    type Error = anyhow::Error;

    async fn server_channel_open_agent_forward(
        &mut self,
        channel: Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<()> {
        if let Some(forward) = &self.agent_forward {
            forward.handle(channel, reply).await;
        } else {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
        }
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<client::Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<()> {
        if let Some(forwards) = &self.remote_forwards {
            forwards
                .handle(
                    channel,
                    connected_address,
                    connected_port,
                    reply,
                    self.network_timeout,
                )
                .await;
        } else {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
        }
        Ok(())
    }

    async fn kex_done(
        &mut self,
        _shared_secret: Option<&[u8]>,
        names: &russh::Names,
        _session: &mut client::Session,
    ) -> Result<()> {
        let _ = self.cipher.set(names.cipher.as_ref().to_owned());
        Ok(())
    }

    async fn check_server_key(&mut self, identity: &PublicKeyOrCertificate) -> Result<bool> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = identity else {
            bail!("Host certificates require a configured CA");
        };
        if let Some(temporary) = &self.temporary_pin {
            ensure!(
                temporary.key_data() == key.key_data(),
                "SSH 连接中的主机密钥已变化"
            );
            return Ok(true);
        }
        let known = {
            let _guard = HOST_KEY_STORE.lock().await;
            self.read_pins()?
        };
        if known
            .iter()
            .any(|(_, saved)| saved.key_data() == key.key_data())
        {
            let _ = self.persistent_key.set(key.clone());
            return Ok(true);
        }
        let previous_fingerprints = known
            .iter()
            .map(|(_, key)| key.fingerprint(HashAlg::Sha256).to_string())
            .collect();

        let (reply, answer) = oneshot::channel();
        (self.progress)(HopProgress {
            index: self.index,
            stage: HopStage::Trust,
            rtt_ms: None,
            authentication: None,
            cipher: self.cipher.get().cloned(),
            error: None,
        });
        self.trust
            .send(ConnectionPrompt::HostKey(TrustRequest {
                host: self.host.clone(),
                port: self.port,
                fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                algorithm: key.algorithm().to_string(),
                known_hosts: self.known_hosts.clone(),
                previous_fingerprints,
                route: self.route.clone(),
                index: self.index,
                reply,
            }))
            .map_err(|_| anyhow::anyhow!("Host key confirmation is unavailable"))?;
        if let Some(signal) = self.awaiting_trust.take() {
            let _ = signal.send(());
        }
        match timeout(TRUST_TIMEOUT, answer)
            .await
            .context("等待确认主机密钥超时")?
            .unwrap_or(TrustDecision::Reject)
        {
            TrustDecision::Reject => bail!("已取消主机密钥确认"),
            TrustDecision::Once => {
                ensure!(known.is_empty(), "主机密钥已变化，需先确认并更新记录");
                let _guard = HOST_KEY_STORE.lock().await;
                let current = self.read_pins()?;
                ensure!(
                    current.is_empty()
                        || current
                            .iter()
                            .any(|(_, saved)| saved.key_data() == key.key_data()),
                    "确认期间主机密钥记录已变化，请重新连接"
                );
                self.temporary_pin = Some(key.clone());
                return Ok(true);
            }
            TrustDecision::Save => {}
        }

        let _guard = HOST_KEY_STORE.lock().await;
        let parent = self
            .known_hosts
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.known_hosts.with_extension("lock"))?;
        lock.try_lock()
            .context("Another CapTerm process is updating the trusted host keys")?;
        let current = self.read_pins()?;
        if !current
            .iter()
            .any(|(_, saved)| saved.key_data() == key.key_data())
        {
            ensure!(
                current
                    .iter()
                    .map(|(_, key)| key.key_data())
                    .eq(known.iter().map(|(_, key)| key.key_data())),
                "保存前主机密钥记录已变化，请重新连接"
            );
            self.write_pin(key, parent, &current)?;
        }
        let _ = self.persistent_key.set(key.clone());
        Ok(true)
    }
}

impl HostVerifier {
    fn read_pins(&self) -> Result<Vec<(usize, PublicKey)>> {
        read_host_pins(&self.host, self.port, &self.known_hosts)
    }

    fn write_pin(&self, key: &PublicKey, parent: &Path, old: &[(usize, PublicKey)]) -> Result<()> {
        let mut contents = match std::fs::read(&self.known_hosts) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error).context("Reading CapTerm trusted hosts"),
        };
        let host = if self.port == 22 {
            self.host.clone()
        } else {
            format!("[{}]:{}", self.host, self.port)
        };
        if !old.is_empty() {
            let old_lines = old.iter().map(|(line, _)| *line).collect::<HashSet<_>>();
            let existing = std::str::from_utf8(&contents).context("读取已保存的主机密钥")?;
            let mut updated = String::new();
            let mut index = 0;
            for line in existing.split_inclusive('\n') {
                // russh numbers non-comment lines when returning key positions.
                if !line.starts_with('#') {
                    index += 1;
                    if old_lines.contains(&index) {
                        let (names, key) = line.split_once(' ').context("主机密钥记录格式无效")?;
                        let aliases = names
                            .split(',')
                            .filter(|name| *name != host)
                            .collect::<Vec<_>>();
                        if aliases.len() == names.split(',').count() {
                            ensure!(
                                !names.contains(','),
                                "请先手动更新包含多个别名的散列主机密钥"
                            );
                        } else if !aliases.is_empty() {
                            updated.push_str(&aliases.join(","));
                            updated.push(' ');
                            updated.push_str(key);
                        }
                        continue;
                    }
                }
                updated.push_str(line);
            }
            contents = updated.into_bytes();
        }
        if !contents.is_empty() && !contents.ends_with(b"\n") {
            contents.push(b'\n');
        }
        writeln!(contents, "{host} {}", key.to_openssh()?)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&contents)?;
        file.as_file().sync_all()?;
        file.persist(&self.known_hosts)
            .context("Saving CapTerm trusted hosts")?;
        Ok(())
    }
}

fn read_host_pins(host: &str, port: u16, path: &Path) -> Result<Vec<(usize, PublicKey)>> {
    // The russh helper treats a file-open error as an empty set of pins.
    match std::fs::File::open(path) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("读取 CapTerm 主机密钥"),
    }
    known_host_keys_path(host, port, path).context("读取 CapTerm 主机密钥")
}

pub struct Connection {
    target: Arc<client::Handle<HostVerifier>>,
    forwarding: Forwarding,
    agent_forward: Option<Arc<AgentForwarding>>,
    jumps: Vec<Arc<SharedHop>>,
    sftp: OnceCell<Arc<SftpSession>>,
    edit_lock: Mutex<()>,
    metric_counters: Mutex<MetricCounters>,
    cipher: Arc<OnceLock<String>>,
    ssh: SshOptions,
}

impl Connection {
    #[cfg(test)]
    pub async fn connect(
        profile: &ConnectionProfile,
        password: Option<String>,
        known_hosts: PathBuf,
        trust: mpsc::UnboundedSender<ConnectionPrompt>,
    ) -> Result<Self> {
        Self::connect_with_progress(profile, password, known_hosts, trust, true, |_| {}).await
    }

    pub async fn connect_with_progress(
        profile: &ConnectionProfile,
        password: Option<String>,
        known_hosts: PathBuf,
        trust: mpsc::UnboundedSender<ConnectionPrompt>,
        reuse_jumps: bool,
        progress: impl Fn(HopProgress) + Send + Sync + 'static,
    ) -> Result<Self> {
        let target = Hop {
            host: profile.host.clone(),
            user: profile.user.clone(),
            port: profile.port,
            identity_files: profile.identity_files.clone(),
            ssh: profile.ssh.clone(),
            reference: None,
        };
        let first = profile.jumps.first().map_or(&target.ssh, |hop| &hop.ssh);
        let proxy_command = first.proxy_command.as_ref();
        let network_proxy = profile
            .ssh
            .network_proxy
            .as_ref()
            .or(first.network_proxy.as_ref());
        ensure!(
            profile.jumps.is_empty() || profile.ssh.proxy_command.is_none(),
            "ProxyCommand 和 ProxyJump 不能同时用于目标会话"
        );
        ensure!(
            proxy_command.is_none() || network_proxy.is_none(),
            "ProxyCommand 和网络代理不能同时用于第一跳"
        );
        ensure!(
            profile
                .jumps
                .iter()
                .skip(1)
                .all(|hop| hop.ssh.proxy_command.is_none() && hop.ssh.network_proxy.is_none()),
            "网络代理和 ProxyCommand 只能用于第一跳"
        );
        let mut sessions: Vec<Arc<SharedHop>> = Vec::new();
        let progress: Arc<dyn Fn(HopProgress) + Send + Sync> = Arc::new(progress);
        let hops = profile.jumps.iter().chain(std::iter::once(&target));
        let route: Vec<_> = hops
            .clone()
            .map(|hop| crate::config::ssh_address(&hop.user, &hop.host, Some(hop.port)))
            .collect();
        let mut upstream_rtt = Some(Duration::ZERO);
        let remote_forwards = Arc::new(RemoteForwards::default());
        let agent_forward = if profile.ssh.forward_agent {
            agent_socket(profile.ssh.identity_agent.as_deref())
                .map(|socket| Arc::new(AgentForwarding::new(socket)))
        } else {
            None
        };
        for (index, hop) in hops.enumerate() {
            let is_target = index == profile.jumps.len();
            let jump_key = if reuse_jumps
                && !is_target
                && proxy_command.is_none()
                && network_proxy.is_none()
            {
                JumpKey::new(&known_hosts, &profile.jumps[..=index])
            } else {
                None
            };
            if let Some(key) = &jump_key
                && let Some(shared) = jumps::reuse(key, sessions.last()).await
            {
                let cumulative = ping_roundtrip(&shared.session, Duration::from_millis(600)).await;
                if !shared.session.is_closed() {
                    progress(HopProgress {
                        index,
                        stage: HopStage::Connected,
                        rtt_ms: segment_ms(cumulative, upstream_rtt),
                        authentication: Some(shared.authentication),
                        cipher: shared.cipher.clone(),
                        error: None,
                    });
                    upstream_rtt = cumulative;
                    sessions.push(shared);
                    continue;
                }
            }
            let cipher = Arc::new(OnceLock::new());
            let persistent_key = Arc::new(OnceLock::new());
            let report = |stage, rtt_ms, authentication, error| {
                progress(HopProgress {
                    index,
                    stage,
                    rtt_ms,
                    authentication,
                    cipher: cipher.get().cloned(),
                    error,
                });
            };
            let endpoint = &route[index];
            let network_timeout = hop
                .ssh
                .connect_timeout
                .filter(|seconds| *seconds != 0)
                .map(|seconds| Duration::from_secs(u64::from(seconds)))
                .unwrap_or(NETWORK_TIMEOUT);
            let compression = if hop.ssh.compression {
                &[russh::compression::ZLIB_LEGACY, russh::compression::NONE][..]
            } else {
                &[russh::compression::NONE][..]
            };
            let config = Arc::new(client::Config {
                keepalive_interval: match hop.ssh.keepalive_interval {
                    Some(0) => None,
                    Some(seconds) => Some(Duration::from_secs(u64::from(seconds))),
                    None => Some(Duration::from_secs(30)),
                },
                preferred: russh::Preferred {
                    compression: Cow::Borrowed(compression),
                    ..Default::default()
                },
                ..Default::default()
            });
            report(HopStage::Connecting, None, None, None);
            let result: Result<_> = async {
                let host = normalize_host(&hop.host)?;
                ensure!(
                    hop.port != 0 && !hop.user.is_empty(),
                    "SSH 用户和端口不能为空"
                );
                let verifier = HostVerifier {
                    host: host.clone(),
                    port: hop.port,
                    known_hosts: known_hosts.clone(),
                    trust: trust.clone(),
                    route: route.clone(),
                    temporary_pin: None,
                    persistent_key: persistent_key.clone(),
                    awaiting_trust: None,
                    index,
                    progress: progress.clone(),
                    cipher: cipher.clone(),
                    agent_forward: if is_target {
                        agent_forward.clone()
                    } else {
                        None
                    },
                    remote_forwards: is_target.then(|| remote_forwards.clone()),
                    network_timeout,
                };
                let mut session = match sessions.last() {
                    Some(previous) => {
                        let stream = timeout(
                            network_timeout,
                            previous.session.channel_open_direct_tcpip(
                                &host,
                                u32::from(hop.port),
                                "127.0.0.1",
                                0,
                            ),
                        )
                        .await
                        .with_context(|| format!("{endpoint} 转发连接超时"))?
                        .with_context(|| {
                            format!("{endpoint} 转发失败，请检查跳板机的 TCP 转发配置")
                        })?
                        .into_stream();
                        connect_transport(config, stream, verifier, network_timeout).await
                    }
                    None => {
                        if let Some(command) = proxy_command {
                            report(HopStage::ProxyApproval, None, None, None);
                            let alias = hop
                                .ssh
                                .proxy_command_alias
                                .as_deref()
                                .unwrap_or(&profile.name);
                            let stream = proxy::run_command(
                                command, &host, hop.port, &hop.user, alias, &trust,
                            )
                            .await?;
                            report(HopStage::Connecting, None, None, None);
                            connect_transport(config, stream, verifier, network_timeout).await
                        } else if let Some(proxy) = network_proxy {
                            let stream =
                                proxy::connect_network(proxy, &host, hop.port, network_timeout)
                                    .await?;
                            stream.set_nodelay(true)?;
                            connect_transport(config, stream, verifier, network_timeout).await
                        } else {
                            let stream = timeout(
                                network_timeout,
                                TcpStream::connect((host.as_str(), hop.port)),
                            )
                            .await
                            .with_context(|| format!("{endpoint} TCP 连接超时"))?
                            .with_context(|| {
                                format!("{endpoint} TCP 连接失败，请检查地址和端口")
                            })?;
                            stream.set_nodelay(true)?;
                            connect_transport(config, stream, verifier, network_timeout).await
                        }
                    }
                }
                .with_context(|| format!("{endpoint} SSH 握手失败"))?;
                report(HopStage::Authenticating, None, None, None);
                let password = if is_target { password.as_deref() } else { None };
                let authentication =
                    timeout(network_timeout, authenticate(&mut session, hop, password))
                        .await
                        .with_context(|| format!("{endpoint} 认证超时"))?
                        .with_context(|| format!("{endpoint} 认证失败，请检查凭据或 SSH Agent"))?;
                Ok((session, authentication, host))
            }
            .await;
            let (session, authentication, host) = match result {
                Ok(result) => result,
                Err(error) => {
                    report(HopStage::Failed, None, None, Some(format!("{error:#}")));
                    return Err(error);
                }
            };
            let cumulative_rtt = ping_roundtrip(&session, Duration::from_millis(600)).await;
            let rtt_ms = segment_ms(cumulative_rtt, upstream_rtt);
            upstream_rtt = cumulative_rtt;
            if is_target {
                let session = Arc::new(session);
                let forwarding = match Forwarding::start(
                    &session,
                    remote_forwards,
                    &profile.ssh.port_forwards,
                    network_timeout,
                )
                .await
                {
                    Ok(forwarding) => forwarding,
                    Err(error) => {
                        report(
                            HopStage::Failed,
                            rtt_ms,
                            Some(authentication),
                            Some(format!("{error:#}")),
                        );
                        let _ = timeout(
                            network_timeout,
                            session.disconnect(
                                Disconnect::ByApplication,
                                "Port forwarding setup failed",
                                "",
                            ),
                        )
                        .await;
                        return Err(error);
                    }
                };
                report(HopStage::Connected, rtt_ms, Some(authentication), None);
                return Ok(Self {
                    target: session,
                    forwarding,
                    agent_forward,
                    jumps: sessions,
                    sftp: OnceCell::new(),
                    edit_lock: Mutex::new(()),
                    metric_counters: Mutex::new(MetricCounters::default()),
                    cipher,
                    ssh: profile.ssh.clone(),
                });
            }
            report(HopStage::Connected, rtt_ms, Some(authentication), None);
            let shared = Arc::new(SharedHop {
                session,
                authentication,
                cipher: cipher.get().cloned(),
                host,
                port: hop.port,
                persistent_key: persistent_key.get().cloned(),
                parent: sessions.last().cloned(),
            });
            if let Some(key) = jump_key {
                jumps::remember(key, &shared);
            }
            sessions.push(shared);
        }
        unreachable!("connection route always includes the target")
    }

    pub fn cipher(&self) -> Option<&str> {
        self.cipher.get().map(String::as_str)
    }

    pub fn forwardings(&self) -> &[PortForward] {
        &self.forwarding.active
    }

    pub async fn disconnect(&self) -> Result<()> {
        if let Some(agent) = &self.agent_forward {
            agent.close();
        }
        self.forwarding.close(&self.target).await;
        timeout(
            NETWORK_TIMEOUT,
            self.target
                .disconnect(Disconnect::ByApplication, "Disconnected", ""),
        )
        .await
        .context("Disconnect timed out")??;
        Ok(())
    }

    pub async fn shell(
        &self,
        columns: u16,
        rows: u16,
    ) -> Result<(mpsc::Sender<ShellCommand>, mpsc::Receiver<ShellEvent>)> {
        let mut channel = self.channel().await?;
        let mut initial = Vec::new();
        let setup = timeout(NETWORK_TIMEOUT, async {
            channel
                .request_pty(
                    true,
                    self.ssh.term.as_deref().unwrap_or("xterm-256color"),
                    u32::from(columns.max(1)),
                    u32::from(rows.max(1)),
                    0,
                    0,
                    &[],
                )
                .await?;
            wait_for_success(&mut channel, &mut initial, "PTY").await?;
            let environment = shell_environment(&self.ssh);
            for (name, value) in environment {
                channel.set_env(false, name, value).await?;
            }
            let agent_lease = if self.ssh.forward_agent {
                let agent = self
                    .agent_forward
                    .as_ref()
                    .context("已启用 SSH Agent 转发，但没有可用的本机 Agent 地址")?;
                let binding = self
                    .target
                    .agent_session_binding()
                    .context("此 SSH 会话不支持安全的 Agent 绑定")?;
                agent.verify(binding).await?;
                channel.agent_forward(true).await?;
                wait_for_success(&mut channel, &mut initial, "SSH Agent 转发").await?;
                Some(agent.activate(binding)?)
            } else {
                None
            };
            channel.request_shell(true).await?;
            wait_for_success(&mut channel, &mut initial, "shell").await?;
            Ok(agent_lease)
        })
        .await
        .context("Opening remote terminal timed out")
        .and_then(|result| result);
        let agent_lease = match setup {
            Ok(agent_lease) => agent_lease,
            Err(error) => {
                close_channel(&channel).await;
                return Err(error);
            }
        };
        let (commands, command_rx) = mpsc::channel(128);
        let (event_tx, events) = mpsc::channel(128);
        tokio::spawn(run_shell(
            channel,
            command_rx,
            event_tx,
            initial,
            agent_lease,
        ));
        Ok((commands, events))
    }

    async fn channel(&self) -> Result<Channel<client::Msg>> {
        Ok(timeout(NETWORK_TIMEOUT, self.target.channel_open_session())
            .await
            .context("Opening SSH channel timed out")??)
    }

    async fn sftp_channel(&self) -> Result<Channel<client::Msg>> {
        let mut channel = self.channel().await?;
        let setup = timeout(NETWORK_TIMEOUT, async {
            channel.request_subsystem(true, "sftp").await?;
            wait_for_success(&mut channel, &mut Vec::new(), "SFTP subsystem").await
        })
        .await
        .context("Opening SFTP timed out")
        .and_then(|result| result);
        if let Err(error) = setup {
            close_channel(&channel).await;
            return Err(error);
        }
        Ok(channel)
    }

    async fn sftp(&self) -> Result<Arc<SftpSession>> {
        self.sftp
            .get_or_try_init(|| async {
                let stream = self.sftp_channel().await?.into_stream();
                let session = SftpSession::new(stream).await.context("Starting SFTP")?;
                Ok(Arc::new(session))
            })
            .await
            .cloned()
    }

    pub async fn list(&self, path: &str) -> Result<(String, Vec<RemoteEntry>)> {
        let sftp = self.sftp().await?;
        let path = sftp.canonicalize(path).await?;
        let mut entries = sftp
            .read_dir(&path)
            .await?
            .filter_map(|entry| {
                let name = entry.file_name();
                if !valid_remote_name(&name) {
                    return None;
                }
                let metadata = entry.metadata();
                Some(RemoteEntry {
                    path: join_remote(&path, &name),
                    hidden: name.starts_with('.'),
                    name,
                    is_dir: entry.file_type().is_dir(),
                    size: metadata.size,
                    mtime: metadata.mtime,
                    permissions: metadata.permissions,
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok((path, entries))
    }

    pub async fn metrics(&self) -> Result<Metrics> {
        let sample_route = async {
            let rtt = ping_roundtrip(&self.target, Duration::from_secs(2)).await;
            let mut cumulative = vec![None; self.jumps.len()];
            // Keep the aggregate probe bounded even if several jump hosts stop responding.
            let _ = timeout(Duration::from_secs(3), async {
                for (index, hop) in self.jumps.iter().enumerate() {
                    cumulative[index] = ping_roundtrip(&hop.session, Duration::from_secs(2)).await;
                }
            })
            .await;
            cumulative.push(rtt);
            let mut previous = Some(Duration::ZERO);
            let segments = cumulative
                .into_iter()
                .map(|current| {
                    let upstream = std::mem::replace(&mut previous, current);
                    segment_ms(current, upstream)
                })
                .collect();
            (rtt.map(roundtrip_ms), segments)
        };
        let probe = async { (self.exec(&METRIC_COMMAND).await, Instant::now()) };
        let ((probe, sampled_at), (rtt_ms, hop_rtt_ms)) = tokio::join!(probe, sample_route);
        let mut metrics = Metrics {
            rtt_ms,
            hop_rtt_ms,
            ..Metrics::default()
        };
        let output = match probe {
            Ok(output) => output,
            Err(_) if rtt_ms.is_some() => return Ok(metrics),
            Err(error) => return Err(error),
        };
        let mut counters = self.metric_counters.lock().await;
        parse_metrics(&output, &mut metrics, &mut counters, sampled_at);
        Ok(metrics)
    }

    pub(crate) async fn exec(&self, command: &str) -> Result<Vec<u8>> {
        let mut channel = self.channel().await?;
        let result = timeout(NETWORK_TIMEOUT, async {
            channel.exec(true, command).await?;
            let mut output = Vec::new();
            wait_for_success(&mut channel, &mut output, "exec").await?;
            let mut exit = None;
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => extend_probe(&mut output, &data)?,
                    ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            ensure!(exit == Some(0), "远程命令执行失败");
            Ok(output)
        })
        .await
        .context("远程命令执行超时");
        close_channel(&channel).await;
        result?
    }
}

async fn ping_roundtrip(
    session: &client::Handle<HostVerifier>,
    wait: Duration,
) -> Option<Duration> {
    let start = Instant::now();
    timeout(wait, session.send_ping()).await.ok()?.ok()?;
    (!session.is_closed()).then(|| start.elapsed())
}

fn roundtrip_ms(elapsed: Duration) -> u64 {
    elapsed.as_millis().try_into().unwrap_or(u64::MAX)
}

fn segment_ms(current: Option<Duration>, upstream: Option<Duration>) -> Option<u64> {
    // Nested sessions measure from the client. Subtract successive round trips
    // to estimate the added latency of each segment.
    Some(roundtrip_ms(current?.checked_sub(upstream?)?))
}

fn normalize_host(host: &str) -> Result<String> {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    ensure!(
        !host.is_empty()
            && host
                .chars()
                .all(|ch| !ch.is_control() && !ch.is_whitespace() && !"[],*?!|#/\\".contains(ch)),
        "Invalid SSH host name"
    );
    Ok(host.to_ascii_lowercase())
}

async fn connect_transport(
    config: Arc<client::Config>,
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    mut verifier: HostVerifier,
    network_timeout: Duration,
) -> Result<client::Handle<HostVerifier>> {
    let (signal, waiting) = oneshot::channel();
    verifier.awaiting_trust = Some(signal);
    let connect = client::connect_stream(config, stream, verifier);
    tokio::pin!(connect);
    tokio::select! {
        result = &mut connect => result,
        result = waiting => {
            let limit = if result.is_ok() { TRUST_TIMEOUT + network_timeout } else { network_timeout };
            timeout(limit, connect).await.context("SSH 握手超时")?
        }
        _ = tokio::time::sleep(network_timeout) => bail!("SSH 握手超时"),
    }
}

fn shell_environment(options: &SshOptions) -> std::collections::BTreeMap<String, String> {
    let mut patterns: Vec<(String, glob::Pattern)> = Vec::new();
    for pattern in &options.send_env {
        if let Some(pattern) = pattern.strip_prefix('-') {
            if let Ok(remove) = glob::Pattern::new(pattern) {
                patterns.retain(|(entry, _)| !remove.matches(entry));
            }
        } else if let Ok(glob) = glob::Pattern::new(pattern) {
            patterns.push((pattern.clone(), glob));
        }
    }
    let mut environment =
        std::collections::BTreeMap::from([("COLORTERM".into(), "truecolor".into())]);
    if !patterns.is_empty() {
        for (name, value) in std::env::vars_os() {
            if let (Ok(name), Ok(value)) = (name.into_string(), value.into_string())
                && patterns.iter().any(|(_, pattern)| pattern.matches(&name))
            {
                environment.insert(name, value);
            }
        }
    }
    environment.extend(options.set_env.clone());
    environment
}

async fn authenticate(
    session: &mut client::Handle<HostVerifier>,
    hop: &Hop,
    password: Option<&str>,
) -> Result<&'static str> {
    let user = &hop.user;
    let mut last_key_error = None;
    // A password provided without a key selection is an explicit choice of authentication method.
    let mut password_tried = false;
    if hop.identity_files.is_empty()
        && let Some(password) = password
    {
        if session
            .authenticate_password(user, password)
            .await?
            .success()
        {
            return Ok("密码");
        }
        password_tried = true;
    }

    let paths = identity_paths(&hop.identity_files);
    let hash = session
        .best_supported_rsa_hash()
        .await?
        .flatten()
        .or(Some(HashAlg::Sha256));
    let mut tried = HashSet::new();
    let mut accepted_key = false;
    for path in &paths {
        let identity = match fs::read_to_string(path).await {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound && hop.identity_files.is_empty() => {
                continue;
            }
            Err(error) => {
                last_key_error = Some(format!("{}: {error}", path.display()));
                continue;
            }
        };
        match decode_secret_key(&identity, None) {
            Ok(key) => {
                if !tried.insert(key.public_key().fingerprint(HashAlg::Sha256).to_string()) {
                    continue;
                }
                let result = session
                    .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                    .await?;
                if result.success() {
                    return Ok("私钥");
                }
                if let client::AuthResult::Failure {
                    partial_success: true,
                    ..
                } = result
                {
                    accepted_key = true;
                    break;
                }
            }
            Err(error) => last_key_error = Some(format!("{}: {error}", path.display())),
        }
    }

    #[cfg(any(windows, unix))]
    if !accepted_key && let Some(socket) = agent_socket(hop.ssh.identity_agent.as_deref()) {
        #[cfg(windows)]
        let agent = timeout(
            Duration::from_secs(1),
            AgentClient::connect_named_pipe(socket),
        )
        .await;
        #[cfg(unix)]
        let agent = timeout(Duration::from_secs(1), AgentClient::connect_uds(socket)).await;
        if let Ok(Ok(agent)) = agent {
            let allowed = if hop.ssh.identities_only {
                Some(agent_identity_fingerprints(&paths).await)
            } else {
                None
            };
            match authenticate_agent(session, user, hash, agent, &mut tried, allowed.as_ref()).await
            {
                Some(client::AuthResult::Success) => return Ok("SSH Agent"),
                Some(client::AuthResult::Failure {
                    partial_success, ..
                }) => accepted_key = partial_success,
                None => {}
            }
        }
    }

    if let Some(password) = password {
        if (!password_tried || accepted_key)
            && session
                .authenticate_password(user, password)
                .await?
                .success()
        {
            return Ok(if accepted_key {
                "公钥 + 密码"
            } else {
                "密码"
            });
        }
        let result = session
            .authenticate_keyboard_interactive_start(user, None::<String>)
            .await?;
        if matches!(result, client::KeyboardInteractiveAuthResponse::Success) {
            return Ok("键盘交互");
        }
        if let client::KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } = result
            && matches!(prompts.as_slice(), [prompt] if !prompt.echo && prompt.prompt.to_ascii_lowercase().contains("password"))
            && matches!(
                session
                    .authenticate_keyboard_interactive_respond(vec![password.to_owned()])
                    .await?,
                client::KeyboardInteractiveAuthResponse::Success
            )
        {
            return Ok("键盘交互");
        }
    }

    match last_key_error {
        Some(error) => bail!(
            "Server rejected available credentials; key could not be loaded ({error}). Encrypted keys can be loaded into an SSH agent."
        ),
        None => bail!("Server rejected available credentials"),
    }
}

#[cfg(any(windows, unix))]
fn agent_socket(configured: Option<&str>) -> Option<PathBuf> {
    let socket = match configured {
        None => {
            #[cfg(windows)]
            return Some(PathBuf::from(r"\\.\pipe\openssh-ssh-agent"));
            #[cfg(unix)]
            return std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from);
        }
        Some(value) if value.eq_ignore_ascii_case("none") => return None,
        Some("SSH_AUTH_SOCK") => std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from)?,
        Some(value) if value.starts_with('$') => {
            let name = value[1..]
                .strip_prefix('{')
                .and_then(|name| name.strip_suffix('}'))
                .unwrap_or(&value[1..]);
            std::env::var_os(name).map(PathBuf::from)?
        }
        Some(value) => identity_paths(&[value.to_owned()]).into_iter().next()?,
    };
    // Avoid opening a remote SMB named pipe when an imported file selects an agent.
    #[cfg(windows)]
    if !socket
        .to_string_lossy()
        .to_ascii_lowercase()
        .starts_with(r"\\.\pipe\")
    {
        return None;
    }
    Some(socket)
}

async fn agent_identity_fingerprints(paths: &[PathBuf]) -> HashSet<String> {
    let mut fingerprints = HashSet::new();
    for path in paths {
        let mut public_path = path.as_os_str().to_owned();
        public_path.push(".pub");
        for candidate in [PathBuf::from(public_path), path.clone()] {
            if let Ok(text) = fs::read_to_string(candidate).await
                && let Ok(key) = PublicKey::from_openssh(&text)
            {
                fingerprints.insert(key.fingerprint(HashAlg::Sha256).to_string());
                break;
            }
        }
    }
    fingerprints
}

fn identity_paths(configured: &[String]) -> Vec<PathBuf> {
    let home = dirs::home_dir();
    if configured.is_empty() {
        return home
            .into_iter()
            .flat_map(|home| {
                ["id_ed25519", "id_ecdsa", "id_rsa"].map(|name| home.join(".ssh").join(name))
            })
            .collect();
    }
    configured
        .iter()
        .filter(|path| !path.eq_ignore_ascii_case("none"))
        .map(|path| {
            let rest = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\"));
            match (rest, &home) {
                (Some(rest), Some(home)) => home.join(rest),
                _ => PathBuf::from(path),
            }
        })
        .collect()
}

async fn authenticate_agent<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    session: &mut client::Handle<HostVerifier>,
    user: &str,
    hash: Option<HashAlg>,
    mut agent: AgentClient<S>,
    tried: &mut HashSet<String>,
    allowed: Option<&HashSet<String>>,
) -> Option<client::AuthResult> {
    if let Some(binding) = session.agent_session_binding()
        && timeout(Duration::from_secs(2), agent.bind_session(binding, false))
            .await
            .is_err()
    {
        return None;
    }
    let Ok(Ok(identities)) = timeout(Duration::from_secs(2), agent.request_identities()).await
    else {
        return None;
    };
    for identity in identities {
        if tried.len() >= 4 {
            break;
        }
        let result = match identity {
            AgentIdentity::PublicKey { key, .. } => {
                let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
                if allowed.is_some_and(|allowed| !allowed.contains(&fingerprint))
                    || !tried.insert(fingerprint)
                {
                    continue;
                }
                session
                    .authenticate_publickey_with(user, key, hash, &mut agent)
                    .await
            }
            AgentIdentity::Certificate { certificate, .. } => {
                let fingerprint = certificate
                    .public_key()
                    .fingerprint(HashAlg::Sha256)
                    .to_string();
                if allowed.is_some_and(|allowed| !allowed.contains(&fingerprint))
                    || !tried.insert(format!("cert:{fingerprint}"))
                {
                    continue;
                }
                session
                    .authenticate_certificate_with(user, certificate, hash, &mut agent)
                    .await
            }
        };
        if let Ok(
            result @ (client::AuthResult::Success
            | client::AuthResult::Failure {
                partial_success: true,
                ..
            }),
        ) = result
        {
            return Some(result);
        }
    }
    None
}

async fn wait_for_success(
    channel: &mut Channel<client::Msg>,
    output: &mut Vec<u8>,
    request: &str,
) -> Result<()> {
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Success => return Ok(()),
            ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                extend_probe(output, &data)?
            }
            ChannelMsg::Failure | ChannelMsg::Close => break,
            _ => {}
        }
    }
    bail!("Server did not accept the {request} request")
}

async fn close_channel(channel: &Channel<client::Msg>) {
    let _ = timeout(Duration::from_secs(2), channel.close()).await;
}

async fn run_shell(
    channel: Channel<client::Msg>,
    mut commands: mpsc::Receiver<ShellCommand>,
    events: mpsc::Sender<ShellEvent>,
    initial: Vec<u8>,
    agent_lease: Option<Arc<AgentLease>>,
) {
    let (mut read, write) = channel.split();
    if !initial.is_empty() && events.send(ShellEvent::Output(initial)).await.is_err() {
        drop(agent_lease);
        let _ = timeout(Duration::from_secs(2), write.close()).await;
        return;
    }
    let result: Result<Option<u32>> = tokio::select! {
        _ = events.closed() => Ok(None),
        result = async {
            while let Some(command) = commands.recv().await {
                match command {
                    ShellCommand::Input(bytes) => write.data_bytes(bytes).await?,
                    ShellCommand::Resize { columns, rows } => {
                        write.window_change(u32::from(columns.max(1)), u32::from(rows.max(1)), 0, 0).await?;
                    }
                    ShellCommand::Close => break,
                }
            }
            Ok(None)
        } => result,
        result = async {
            let mut status = None;
            while let Some(message) = read.wait().await {
                let output = match message {
                    ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                        Some(data)
                    }
                    ChannelMsg::ExitStatus { exit_status } => {
                        status = Some(exit_status);
                        None
                    }
                    ChannelMsg::ExitSignal { signal_name, .. } => bail!("Remote shell exited with signal {signal_name:?}"),
                    ChannelMsg::Close => break,
                    _ => None,
                };
                if let Some(data) = output
                    && events.send(ShellEvent::Output(data.to_vec())).await.is_err()
                {
                    break;
                }
            }
            Ok(status)
        } => result,
    };
    drop(agent_lease);
    let _ = timeout(Duration::from_secs(2), write.close()).await;
    let _ = events
        .send(match result {
            Ok(status) => ShellEvent::Exited(status),
            Err(error) => ShellEvent::Error(error.to_string()),
        })
        .await;
}

struct RemoteStage {
    sftp: Arc<SftpSession>,
    path: String,
    file: Option<russh_sftp::client::fs::File>,
    committed: bool,
}

impl RemoteStage {
    async fn create(sftp: Arc<SftpSession>, path: String) -> Result<Self> {
        let file = sftp
            .open_with_flags_and_attributes(
                &path,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
                FileAttributes {
                    permissions: Some(0o600),
                    ..FileAttributes::empty()
                },
            )
            .await
            .context("Creating temporary remote file")?;
        Ok(Self {
            sftp,
            path,
            file: Some(file),
            committed: false,
        })
    }

    fn file(&mut self) -> &mut russh_sftp::client::fs::File {
        self.file
            .as_mut()
            .expect("remote staging file is still open")
    }

    async fn finish_writing(&mut self) -> Result<()> {
        let file = self.file();
        file.flush().await?;
        file.sync_all().await?;
        self.file
            .take()
            .expect("remote staging file is still open")
            .close()
            .await?;
        Ok(())
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for RemoteStage {
    fn drop(&mut self) {
        if !self.committed
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let sftp = self.sftp.clone();
            let path = std::mem::take(&mut self.path);
            let file = self.file.take();
            runtime.spawn(async move {
                if let Some(file) = file {
                    let _ = file.close().await;
                }
                let _ = sftp.remove_file(path).await;
            });
        }
    }
}

fn valid_remote_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

pub(crate) fn valid_local_name(name: &str) -> bool {
    if !valid_remote_name(name) {
        return false;
    }
    #[cfg(windows)]
    {
        if name.ends_with(['.', ' '])
            || name
                .chars()
                .any(|c| c.is_control() || r#"\:<>"|?*"#.contains(c))
        {
            return false;
        }
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end()
            .to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        {
            return false;
        }
    }
    true
}

fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn remote_temp(destination: &str) -> String {
    let name = format!(".capterm-{}.part", uuid::Uuid::new_v4());
    match destination.rsplit_once('/') {
        Some((dir, _)) => join_remote(dir, &name),
        None => name,
    }
}

async fn posix_rename(raw: &RawSftpSession, source: &str, destination: &str) -> Result<()> {
    let mut arguments = Vec::new();
    for path in [source, destination] {
        arguments.extend_from_slice(&u32::try_from(path.len())?.to_be_bytes());
        arguments.extend_from_slice(path.as_bytes());
    }
    match raw.extended(POSIX_RENAME, arguments).await? {
        Packet::Status(status) if status.status_code == StatusCode::Ok => Ok(()),
        Packet::Status(status) => bail!("Remote file replacement failed: {:?}", status.status_code),
        _ => bail!("Unexpected response to remote file replacement"),
    }
}

fn extend_probe(output: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    ensure!(
        output.len() + bytes.len() <= MAX_PROBE_BYTES,
        "SSH channel output exceeded the probe limit"
    );
    output.extend_from_slice(bytes);
    Ok(())
}

static METRIC_COMMAND: LazyLock<String> = LazyLock::new(|| {
    let script = include_str!("ssh/metrics.sh").replace("\r\n", "\n");
    format!("sh -c '{}'", script.replace('\'', "'\\''"))
});

fn parse_metrics(
    output: &[u8],
    metrics: &mut Metrics,
    counters: &mut MetricCounters,
    sampled_at: Instant,
) {
    let output = String::from_utf8_lossy(output);
    let mut interfaces = HashMap::new();
    for line in output.lines() {
        let Some((name, value)) = line.split_once(' ') else {
            continue;
        };
        match name {
            "platform" => {
                metrics.probe_available = true;
                metrics.unsupported = !matches!(
                    value,
                    "Linux" | "Darwin" | "FreeBSD" | "OpenBSD" | "NetBSD" | "DragonFly"
                );
            }
            "os" => {
                metrics.os = metric_label(value.trim_matches(['\'', '"']), 80);
            }
            "kernel" => metrics.kernel = metric_label(value, 80),
            "cores" => metrics.cores = value.parse().ok().filter(|&cores| cores > 0),
            "cpu" => {
                if let Some((total, idle)) = parse_pair(value) {
                    let previous = counters.cpu.replace((total, idle));
                    if let Some((last_total, last_idle)) = previous
                        && let (Some(total), Some(idle)) =
                            (total.checked_sub(last_total), idle.checked_sub(last_idle))
                        && total > 0
                        && idle <= total
                    {
                        metrics.cpu = Some((total - idle) as f64 * 100.0 / total as f64);
                    }
                }
            }
            "cpu_percent" => metrics.cpu = parse_percent(value),
            "memory" => metrics.memory = parse_usage(value),
            "swap" => metrics.swap = parse_usage(value),
            "disk" => {
                if let Some(disk) = parse_disk(value) {
                    metrics.disks.push(disk);
                }
            }
            "net" => {
                if let Some((interface, bytes)) = value.split_once(' ')
                    && let Some(counters) = parse_pair(bytes)
                {
                    interfaces.insert(interface.to_owned(), counters);
                }
            }
            "load" => {
                metrics.load = (|| {
                    let mut values = value.split_ascii_whitespace().map(nonnegative_f64);
                    Some([values.next()??, values.next()??, values.next()??])
                })();
            }
            "uptime" => metrics.uptime_secs = value.parse().ok(),
            "process" => {
                if let Some(process) = parse_process(value) {
                    metrics.processes.push(process);
                }
            }
            _ => {}
        }
    }
    metrics.disks.sort_by(|a, b| a.mount.cmp(&b.mount));
    metrics.processes.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
    if !interfaces.is_empty() {
        let current = NetworkCounters {
            sampled_at,
            interfaces,
        };
        if let Some(previous) = counters.network.as_ref() {
            metrics.network_bps = network_rate(&current, previous);
        }
        counters.network = Some(current);
    }
}

fn network_rate(current: &NetworkCounters, previous: &NetworkCounters) -> Option<(f64, f64)> {
    let seconds = current
        .sampled_at
        .checked_duration_since(previous.sampled_at)?
        .as_secs_f64();
    if seconds == 0. {
        return None;
    }
    let mut deltas = current.interfaces.iter().filter_map(|(name, &(rx, tx))| {
        let &(old_rx, old_tx) = previous.interfaces.get(name)?;
        Some((
            rx.checked_sub(old_rx)? as f64,
            tx.checked_sub(old_tx)? as f64,
        ))
    });
    let first = deltas.next()?;
    let (rx, tx) = deltas.fold(first, |(rx, tx), (a, b)| (rx + a, tx + b));
    Some((rx / seconds, tx / seconds))
}

fn metric_label(value: &str, limit: usize) -> Option<String> {
    let value: String = value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(limit)
        .collect();
    (!value.is_empty()).then_some(value)
}

fn parse_usage(value: &str) -> Option<Usage> {
    let (total, available) = parse_pair(value)?;
    let used = total.checked_sub(available)?;
    Some(Usage {
        used,
        total,
        percent: if total == 0 {
            0.
        } else {
            used as f64 * 100. / total as f64
        },
    })
}

fn parse_disk(value: &str) -> Option<DiskUsage> {
    let mut fields = value.splitn(4, ' ');
    let percent = parse_percent(fields.next()?)?;
    let used = fields.next()?.parse().ok()?;
    let total = fields.next()?.parse().ok()?;
    let mount = metric_label(fields.next()?, 256)?;
    Some(DiskUsage {
        mount,
        usage: Usage {
            used,
            total,
            percent,
        },
    })
}

fn parse_process(mut value: &str) -> Option<ProcessSample> {
    let mut next = || {
        let (first, rest) = value.trim_start().split_once(char::is_whitespace)?;
        value = rest;
        Some(first)
    };
    let pid = next()?.parse().ok().filter(|&pid| pid > 0)?;
    let cpu = nonnegative_f64(next()?)?;
    let memory = parse_percent(next()?)?;
    Some(ProcessSample {
        pid,
        cpu,
        memory,
        command: metric_label(value, 180)?,
    })
}

fn nonnegative_f64(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.)
}

fn parse_percent(value: &str) -> Option<f64> {
    Some(nonnegative_f64(value.trim_end_matches('%'))?.min(100.))
}

fn parse_pair(value: &str) -> Option<(u64, u64)> {
    let mut values = value.split_ascii_whitespace();
    Some((values.next()?.parse().ok()?, values.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::{
        ChannelId, Pty,
        keys::{
            PrivateKey,
            ssh_key::{
                LineEnding,
                private::{Ed25519Keypair, KeypairData},
            },
        },
        server::{self, Auth, ChannelOpenHandle, Msg, Session, VerifiedPublicKey},
    };
    use russh_sftp::protocol::{Attrs, Data, File as SftpFile, Handle, Name, Status, Version};
    use tokio::{io::AsyncReadExt, net::TcpListener, sync::RwLock, task::JoinHandle};

    use crate::file_text::{SaveTextResult, TextEncoding, preserve_line_endings};

    const METRIC_SAMPLE: &[u8] = b"platform Linux\nos Test Linux\ncpu 1000000000000 500000000000\nmemory 1048576 262144\nswap 0 0\nload 0.42 0.44 0.48\nuptime 3600\ndisk 68% 680000 1000000 /volume with spaces\nnet en0 10000 20000\nprocess 12 125.3 8.5 build worker\n";
    const FIXTURE_TEXT: &str = "capterm\r\n你好\r\n";
    const FIXTURE_PATH: &str = "/fixture/notes.txt";
    const FIXTURE_LARGE_PATH: &str = "/fixture/large.log";
    const FIXTURE_LARGE_SIZE: u64 = 64 * crate::file_text::MEBIBYTE;
    const FIXTURE_MTIME: u32 = 1_700_000_001;

    #[test]
    fn metrics_use_deltas_and_report_partial_results() {
        let mut counters = MetricCounters::default();
        let start = Instant::now();
        let mut metrics = Metrics::default();
        parse_metrics(METRIC_SAMPLE, &mut metrics, &mut counters, start);
        assert_eq!(metrics.cpu, None);
        assert_eq!(metrics.network_bps, None);
        let memory = metrics.memory.unwrap();
        assert_eq!(memory.percent, 75.0);
        assert_eq!(memory.total, 1_048_576);
        assert_eq!(memory.used, 786_432);
        assert_eq!(metrics.swap.unwrap().percent, 0.0);
        assert_eq!(metrics.disks[0].usage.percent, 68.0);
        assert_eq!(metrics.disks[0].mount, "/volume with spaces");
        assert_eq!(metrics.processes[0].cpu, 125.3);
        assert_eq!(metrics.processes[0].command, "build worker");
        assert_eq!(metrics.load, Some([0.42, 0.44, 0.48]));
        assert_eq!(metrics.os.as_deref(), Some("Test Linux"));

        let mut metrics = Metrics::default();
        parse_metrics(
            b"cpu 1000000000100 500000000020\nload NaN\nnet en0 10500 20100\nnet en1 999999 888888\n",
            &mut metrics,
            &mut counters,
            start + Duration::from_secs(5),
        );
        assert_eq!(metrics.cpu, Some(80.0));
        assert_eq!(metrics.network_bps, Some((100., 20.)));
        assert_eq!(metrics.load, None);

        let mut metrics = Metrics::default();
        parse_metrics(
            b"cpu 20 10\nnet en0 20 10\n",
            &mut metrics,
            &mut counters,
            start + Duration::from_secs(10),
        );
        assert_eq!(metrics.cpu, None);
        assert_eq!(metrics.network_bps, None);
    }

    #[test]
    fn preserves_crlf_when_saving_edited_text() {
        assert_eq!(
            preserve_line_endings("before\r\n", "after\nline\r\n"),
            "after\r\nline\r\n"
        );
        assert_eq!(preserve_line_endings("before\n", "after\n"), "after\n");
    }

    #[cfg(windows)]
    #[test]
    fn rejects_windows_aliases_and_alternate_streams() {
        for bad in [
            "..",
            r"dir\payload",
            "AUX.log",
            "LPT¹.txt",
            "name.",
            "note:stream",
        ] {
            assert!(!valid_local_name(bad), "unexpectedly accepted {bad:?}");
        }
        assert!(valid_local_name("普通.txt"));
    }

    #[tokio::test]
    async fn trusting_a_host_once_only_pins_the_active_connection() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let known_hosts = temp.path().join("known_hosts");
        let (trust, mut requests) = mpsc::unbounded_channel::<ConnectionPrompt>();
        let mut verifier = HostVerifier {
            host: "fixture".into(),
            port: 22,
            known_hosts: known_hosts.clone(),
            trust,
            route: vec!["fixture@fixture:22".into()],
            temporary_pin: None,
            persistent_key: Arc::new(OnceLock::new()),
            awaiting_trust: None,
            index: 0,
            progress: Arc::new(|_| {}),
            cipher: Arc::new(OnceLock::new()),
            agent_forward: None,
            remote_forwards: None,
            network_timeout: NETWORK_TIMEOUT,
        };
        let first = PublicKeyOrCertificate::from(fixture_key(20)?.public_key().clone());
        let approve = tokio::spawn(async move {
            let ConnectionPrompt::HostKey(request) =
                requests.recv().await.expect("first host key prompt")
            else {
                panic!("expected host key prompt")
            };
            assert!(request.previous_fingerprints.is_empty());
            request.reply.send(TrustDecision::Once).unwrap();
            requests
        });
        assert!(client::Handler::check_server_key(&mut verifier, &first).await?);
        let mut requests = approve.await?;
        assert!(!known_hosts.exists());
        assert!(verifier.persistent_key.get().is_none());
        assert!(client::Handler::check_server_key(&mut verifier, &first).await?);
        let changed = PublicKeyOrCertificate::from(fixture_key(21)?.public_key().clone());
        assert!(
            client::Handler::check_server_key(&mut verifier, &changed)
                .await
                .is_err()
        );
        assert!(requests.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn connects_through_two_jumps_and_requires_confirmation_for_a_changed_key() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let known_hosts = temp.path().join("known_hosts");
        let client_key = fixture_key(10)?;
        let key_path = temp.path().join("test_ed25519");
        fs::write(&key_path, client_key.to_openssh(LineEnding::LF)?.as_bytes()).await?;
        let (jump_a, _, _jump_a_task) = start_fixture(20, client_key.public_key().clone()).await?;
        let (jump_b, _, _jump_b_task) = start_fixture(21, client_key.public_key().clone()).await?;
        let (target, target_key, _target_task) =
            start_fixture(22, client_key.public_key().clone()).await?;
        let profile = ConnectionProfile {
            id: "fixture".into(),
            name: "fixture".into(),
            group: String::new(),
            ssh: SshOptions::default(),
            host: "127.0.0.1".into(),
            user: "fixture".into(),
            port: target,
            identity_files: Vec::new(),
            jumps: [jump_a, jump_b]
                .into_iter()
                .map(|port| Hop {
                    host: "127.0.0.1".into(),
                    user: "fixture".into(),
                    port,
                    identity_files: vec![key_path.to_string_lossy().into_owned()],
                    ssh: SshOptions::default(),
                    reference: None,
                })
                .collect(),
            source: None,
        };
        let (trust, mut requests) = mpsc::unbounded_channel::<ConnectionPrompt>();
        let approve = tokio::spawn(async move {
            for port in [jump_a, jump_b, target] {
                let ConnectionPrompt::HostKey(request) =
                    requests.recv().await.expect("host key prompt")
                else {
                    panic!("expected host key prompt")
                };
                assert_eq!(request.port, port);
                assert!(request.fingerprint.starts_with("SHA256:"));
                request.reply.send(TrustDecision::Save).unwrap();
            }
            requests
        });
        let route = Arc::new(std::sync::Mutex::new(Vec::new()));
        let progress = route.clone();
        let connect = Connection::connect_with_progress(
            &profile,
            Some("fixture-password".into()),
            known_hosts.clone(),
            trust.clone(),
            true,
            move |hop| progress.lock().unwrap().push(hop),
        );
        let connection = timeout(Duration::from_secs(10), connect).await??;
        let mut requests = approve.await?;
        {
            let route = route.lock().unwrap();
            for (index, authentication) in ["私钥", "私钥", "密码"].into_iter().enumerate() {
                let hops: Vec<_> = route.iter().filter(|hop| hop.index == index).collect();
                assert_eq!(
                    hops.iter().map(|hop| hop.stage).collect::<Vec<_>>(),
                    [
                        HopStage::Connecting,
                        HopStage::Trust,
                        HopStage::Authenticating,
                        HopStage::Connected,
                    ]
                );
                assert_eq!(hops[3].authentication, Some(authentication));
                assert!(hops[3].cipher.is_some());
            }
        }
        let (commands, mut output) = connection.shell(80, 24).await?;
        let error = connection
            .list(".")
            .await
            .expect_err("fixture rejects its first SFTP request");
        assert!(error.to_string().contains("SFTP subsystem"));
        let (directory, entries) = connection.list(".").await?;
        assert_eq!(directory, "/fixture");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].path, "/fixture/config");
        assert_eq!(entries[1].path, FIXTURE_PATH);
        assert_eq!(entries[1].mtime, Some(FIXTURE_MTIME));
        assert_eq!(entries[1].size, Some(FIXTURE_TEXT.len() as u64));
        let text = connection
            .open_remote_text(FIXTURE_PATH, TextEncoding::Utf8, 5)
            .await?
            .context("fixture text must open in the editor")?;
        assert_eq!(text.text, FIXTURE_TEXT);
        assert!(!text.readonly);
        let preview = connection
            .open_remote_text(FIXTURE_LARGE_PATH, TextEncoding::Utf8, 5)
            .await?
            .context("large file must open as a preview")?;
        assert!(preview.readonly && preview.truncated);
        assert_eq!(preview.size, Some(FIXTURE_LARGE_SIZE));
        assert_eq!(preview.text.len(), crate::file_text::PREVIEW_BYTES as usize);
        let destination = temp.path().join("notes.txt");
        fs::write(&destination, "existing").await?;
        let (_signal, watch) = tokio::sync::watch::channel(TransferSignal::Running);
        let mut options = TransferOptions {
            choice: TransferChoice::Ask,
            preserve_metadata: true,
        };
        let conflict = connection
            .download_file(
                FIXTURE_PATH,
                &destination,
                options,
                watch.clone(),
                |_, _| {},
            )
            .await?;
        assert!(matches!(conflict, TransferOutcome::Conflict(_)));
        options.choice = TransferChoice::KeepBoth;
        let copied = connection
            .download_file(FIXTURE_PATH, &destination, options, watch, |_, _| {})
            .await?;
        let TransferOutcome::Complete(path) = copied else {
            bail!("expected a second copy of the download");
        };
        assert_eq!(Path::new(&path), temp.path().join("notes (1).txt"));
        assert_eq!(fs::read_to_string(&path).await?, FIXTURE_TEXT);
        assert_eq!(fs::read_to_string(&destination).await?, "existing");
        let modified = fs::metadata(&path)
            .await?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?;
        assert_eq!(modified.as_secs(), u64::from(FIXTURE_MTIME));
        let (_signal, watch) = tokio::sync::watch::channel(TransferSignal::Cancelled);
        let cancelled = temp.path().join("cancelled.txt");
        let result = connection
            .download_file(FIXTURE_PATH, &cancelled, options, watch, |_, _| {})
            .await?;
        assert!(matches!(result, TransferOutcome::Cancelled));
        assert!(!cancelled.exists());

        let external = "changes from another editor\n";
        assert!(matches!(
            connection
                .save_remote_text(FIXTURE_PATH, &text, external, false)
                .await?,
            SaveTextResult::Saved(_)
        ));
        let stale = "changes from this editor\n";
        let conflict = connection
            .save_remote_text(FIXTURE_PATH, &text, stale, false)
            .await?;
        let SaveTextResult::Conflict(Some(current)) = conflict else {
            bail!("expected a conflict from a stale editor");
        };
        assert_eq!(current.text, "changes from another editor\r\n");
        let result = connection
            .save_remote_text(FIXTURE_PATH, &text, stale, true)
            .await?;
        let SaveTextResult::Saved(saved) = result else {
            bail!("expected atomic save after conflict override");
        };
        assert_eq!(saved.text, "changes from this editor\r\n");
        let metadata = connection
            .sftp()
            .await?
            .symlink_metadata(FIXTURE_PATH)
            .await?;
        assert_eq!(metadata.permissions, Some(0o100640));
        assert_eq!((metadata.uid, metadata.gid), (Some(1000), Some(1001)));

        let (_signal, watch) = tokio::sync::watch::channel(TransferSignal::Running);
        let copied = connection
            .upload_file(&destination, "/fixture", options, watch, |_, _| {})
            .await?;
        let TransferOutcome::Complete(path) = copied else {
            bail!("expected a second remote copy");
        };
        assert_eq!(path, "/fixture/notes (1).txt");
        let added = connection
            .open_remote_text(&path, TextEncoding::Utf8, 5)
            .await?
            .context("read uploaded file")?;
        assert_eq!(added.text, "existing");
        commands
            .send(ShellCommand::Resize {
                columns: 90,
                rows: 36,
            })
            .await?;
        expect_output(&mut output, b"90x36").await?;
        commands
            .send(ShellCommand::Input(b"echo fixture\n".to_vec()))
            .await?;
        expect_output(&mut output, b"echo fixture\n").await?;
        let metrics = connection.metrics().await?;
        assert_eq!(metrics.memory.unwrap().percent, 75.0);
        assert!(metrics.rtt_ms.is_some());
        assert_eq!(metrics.hop_rtt_ms.len(), 3);
        assert!(connection.cipher().is_some());

        let (second_target, _, _second_target_task) =
            start_fixture(24, client_key.public_key().clone()).await?;
        let second_profile = ConnectionProfile {
            port: second_target,
            ..profile.clone()
        };
        let approve = tokio::spawn(async move {
            let ConnectionPrompt::HostKey(request) = requests
                .recv()
                .await
                .expect("second target host key prompt")
            else {
                panic!("expected host key prompt")
            };
            assert_eq!(request.port, second_target);
            request.reply.send(TrustDecision::Save).unwrap();
            requests
        });
        let second = timeout(
            Duration::from_secs(10),
            Connection::connect(
                &second_profile,
                Some("fixture-password".into()),
                known_hosts.clone(),
                trust.clone(),
            ),
        )
        .await??;
        let requests = approve.await?;
        for (a, b) in connection.jumps.iter().zip(&second.jumps) {
            assert!(Arc::ptr_eq(a, b));
        }
        connection.disconnect().await?;
        drop(connection);
        let second_metrics = second.metrics().await?;
        assert!(second_metrics.rtt_ms.is_some());
        assert_eq!(second_metrics.memory.unwrap().percent, 75.0);

        let disabled = timeout(
            Duration::from_secs(10),
            Connection::connect_with_progress(
                &second_profile,
                Some("fixture-password".into()),
                known_hosts.clone(),
                trust.clone(),
                false,
                |_| {},
            ),
        )
        .await??;
        for (a, b) in second.jumps.iter().zip(&disabled.jumps) {
            assert!(!Arc::ptr_eq(a, b));
        }
        disabled.disconnect().await?;
        drop(disabled);

        let pins = fs::read_to_string(&known_hosts).await?;
        fs::write(&known_hosts, "").await?;
        let cache_key = JumpKey::new(&known_hosts, &profile.jumps[..1]).unwrap();
        assert!(jumps::reuse(&cache_key, None).await.is_none());
        fs::write(&known_hosts, pins).await?;
        second.disconnect().await?;
        drop(second);

        *target_key.write().await = fixture_key(23)?;
        let mut requests = requests;
        let decline = tokio::spawn(async move {
            let ConnectionPrompt::HostKey(request) =
                requests.recv().await.expect("changed host key prompt")
            else {
                panic!("expected host key prompt")
            };
            assert_eq!(request.port, target);
            assert_eq!(request.previous_fingerprints.len(), 1);
            request.reply.send(TrustDecision::Reject).unwrap();
            requests
        });
        route.lock().unwrap().clear();
        let progress = route.clone();
        let reconnect = Connection::connect_with_progress(
            &profile,
            Some("fixture-password".into()),
            known_hosts.clone(),
            trust.clone(),
            true,
            move |hop| progress.lock().unwrap().push(hop),
        );
        let error = match timeout(Duration::from_secs(10), reconnect).await? {
            Ok(_) => bail!("Changed host key was accepted"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("已取消主机密钥确认"));
        {
            let route = route.lock().unwrap();
            let failed = route.last().unwrap();
            assert_eq!(failed.index, 2);
            assert_eq!(failed.stage, HopStage::Failed);
            assert!(
                failed
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("已取消主机密钥确认")
            );
        }
        let mut requests = decline.await?;
        let accept = tokio::spawn(async move {
            let ConnectionPrompt::HostKey(request) =
                requests.recv().await.expect("changed host key prompt")
            else {
                panic!("expected host key prompt")
            };
            assert_eq!(request.previous_fingerprints.len(), 1);
            request.reply.send(TrustDecision::Save).unwrap();
            requests
        });
        let reconnect = Connection::connect(
            &profile,
            Some("fixture-password".into()),
            known_hosts.clone(),
            trust.clone(),
        );
        timeout(Duration::from_secs(10), reconnect)
            .await??
            .disconnect()
            .await?;
        let mut requests = accept.await?;
        let reconnect = Connection::connect(
            &profile,
            Some("fixture-password".into()),
            known_hosts,
            trust,
        );
        timeout(Duration::from_secs(10), reconnect)
            .await??
            .disconnect()
            .await?;
        assert!(requests.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn forwards_local_remote_and_socks_streams_until_disconnect() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (ssh_port, _, server) =
            start_fixture(73, fixture_key(74)?.public_key().clone()).await?;
        let destination = TcpListener::bind(("127.0.0.1", 0)).await?;
        let echo_port = destination.local_addr()?.port();
        let echo = tokio::spawn(async move {
            while let Ok((socket, _)) = destination.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = socket.into_split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });
        let destination = format!("127.0.0.1:{echo_port}");
        let profile = ConnectionProfile {
            id: "forward-fixture".into(),
            name: "forward-fixture".into(),
            host: "127.0.0.1".into(),
            user: "fixture".into(),
            port: ssh_port,
            identity_files: Vec::new(),
            jumps: Vec::new(),
            source: None,
            group: String::new(),
            ssh: SshOptions {
                port_forwards: vec![
                    PortForward::parse("LocalForward", &["0", &destination])?,
                    PortForward::parse("RemoteForward", &["0", &destination])?,
                    PortForward::parse("DynamicForward", &["0"])?,
                ],
                ..SshOptions::default()
            },
        };
        let (trust, mut requests) = mpsc::unbounded_channel();
        let approve = tokio::spawn(async move {
            if let Some(ConnectionPrompt::HostKey(request)) = requests.recv().await {
                let _ = request.reply.send(TrustDecision::Once);
            }
        });
        let connection = timeout(
            Duration::from_secs(10),
            Connection::connect(
                &profile,
                Some("fixture-password".into()),
                directory.path().join("known_hosts"),
                trust,
            ),
        )
        .await??;
        approve.await?;
        let ports: Vec<_> = connection
            .forwardings()
            .iter()
            .map(PortForward::listen_port)
            .collect();
        assert_eq!(ports.len(), 3);
        let transfer = async {
            for (index, &port) in ports.iter().enumerate() {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
                if index == 2 {
                    stream.write_all(&[5, 1, 0]).await?;
                    let mut greeting = [0; 2];
                    stream.read_exact(&mut greeting).await?;
                    assert_eq!(greeting, [5, 0]);
                    let [high, low] = echo_port.to_be_bytes();
                    stream
                        .write_all(&[5, 1, 0, 1, 127, 0, 0, 1, high, low])
                        .await?;
                    let mut reply = [0; 10];
                    stream.read_exact(&mut reply).await?;
                    assert_eq!(&reply[..4], &[5, 0, 0, 1]);
                }
                let data = [b'f', b'w', b'd', index as u8];
                stream.write_all(&data).await?;
                let mut returned = [0; 4];
                stream.read_exact(&mut returned).await?;
                assert_eq!(returned, data);
            }
            Ok::<_, anyhow::Error>(())
        };
        timeout(Duration::from_secs(5), transfer)
            .await
            .context("Timed out relaying forwarded traffic")??;
        connection.disconnect().await?;
        for &port in &ports {
            timeout(Duration::from_secs(3), async {
                while TcpListener::bind(("127.0.0.1", port)).await.is_err() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .with_context(|| format!("Forwarding port {port} was still bound after disconnect"))?;
        }
        server.abort();
        echo.abort();
        Ok(())
    }

    async fn expect_output(output: &mut mpsc::Receiver<ShellEvent>, expected: &[u8]) -> Result<()> {
        let mut bytes = Vec::new();
        timeout(Duration::from_secs(5), async {
            while bytes.len() < expected.len() {
                match output.recv().await {
                    Some(ShellEvent::Output(data)) => bytes.extend_from_slice(&data),
                    _ => bail!("Terminal closed while waiting for fixture output"),
                }
            }
            assert_eq!(bytes, expected);
            Ok(())
        })
        .await?
    }

    #[tokio::test]
    async fn forwards_only_a_bound_agent_during_an_enabled_shell() -> Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let temp = tempfile::tempdir()?;
        let known_hosts = temp.path().join("known_hosts");
        let bindings = Arc::new(AtomicUsize::new(0));
        let (socket, mock) = start_mock_agent(temp.path(), bindings.clone())?;
        let (port, _, _server) = start_fixture(26, fixture_key(10)?.public_key().clone()).await?;
        let mut profile = ConnectionProfile {
            id: "agent-fixture".into(),
            name: "agent-fixture".into(),
            group: String::new(),
            host: "127.0.0.1".into(),
            user: "fixture".into(),
            port,
            identity_files: Vec::new(),
            jumps: Vec::new(),
            source: None,
            ssh: SshOptions {
                identity_agent: Some(socket),
                ..SshOptions::default()
            },
        };
        let (trust, mut requests) = mpsc::unbounded_channel::<ConnectionPrompt>();
        let approve = tokio::spawn(async move {
            let Some(ConnectionPrompt::HostKey(request)) = requests.recv().await else {
                panic!("expected host key prompt");
            };
            request.reply.send(TrustDecision::Save).unwrap();
        });
        let disabled = timeout(
            Duration::from_secs(10),
            Connection::connect(
                &profile,
                Some("fixture-password".into()),
                known_hosts.clone(),
                trust.clone(),
            ),
        )
        .await??;
        approve.await?;
        let (_commands, _output) = disabled.shell(80, 24).await?;
        assert_eq!(disabled.exec("fixture-agent").await?, b"blocked");
        disabled.disconnect().await?;

        profile.ssh.forward_agent = true;
        let enabled = timeout(
            Duration::from_secs(10),
            Connection::connect(
                &profile,
                Some("fixture-password".into()),
                known_hosts,
                trust,
            ),
        )
        .await??;
        assert_eq!(enabled.exec("fixture-agent").await?, b"blocked");
        assert_eq!(bindings.load(Ordering::Relaxed), 0);

        let (commands, mut output) = enabled.shell(80, 24).await?;
        assert_eq!(enabled.exec("fixture-agent").await?, b"relayed");
        assert_eq!(bindings.load(Ordering::Relaxed), 2);
        commands.send(ShellCommand::Close).await?;
        assert!(matches!(
            timeout(Duration::from_secs(5), output.recv()).await?,
            Some(ShellEvent::Exited(_))
        ));
        assert_eq!(enabled.exec("fixture-agent").await?, b"blocked");
        enabled.disconnect().await?;
        mock.abort();
        Ok(())
    }

    fn start_mock_agent(
        directory: &Path,
        bindings: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Result<(String, JoinHandle<()>)> {
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let _ = directory;
            let address = format!(r"\\.\pipe\capterm-test-agent-{}", uuid::Uuid::new_v4());
            let name = address.clone();
            let mut pipe = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)?;
            let task = tokio::spawn(async move {
                while pipe.connect().await.is_ok() {
                    let Ok(next) = ServerOptions::new().create(&name) else {
                        break;
                    };
                    let stream = std::mem::replace(&mut pipe, next);
                    let bindings = bindings.clone();
                    tokio::spawn(async move {
                        let _ = serve_mock_agent(stream, bindings).await;
                    });
                }
            });
            Ok((address, task))
        }
        #[cfg(unix)]
        {
            let socket = directory.join("agent.sock");
            let listener = tokio::net::UnixListener::bind(&socket)?;
            let task = tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let bindings = bindings.clone();
                    tokio::spawn(async move {
                        let _ = serve_mock_agent(stream, bindings).await;
                    });
                }
            });
            Ok((socket.to_string_lossy().into_owned(), task))
        }
    }

    async fn serve_mock_agent(
        mut stream: impl AsyncRead + AsyncWrite + Unpin,
        bindings: Arc<std::sync::atomic::AtomicUsize>,
    ) -> std::io::Result<()> {
        let length = stream.read_u32().await? as usize;
        let mut binding = vec![0; length];
        stream.read_exact(&mut binding).await?;
        let extension = b"session-bind@openssh.com";
        assert_eq!(binding[0], 27);
        assert_eq!(&binding[5..5 + extension.len()], extension);
        assert_eq!(binding.last(), Some(&1));
        bindings.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        stream.write_all(&[0, 0, 0, 1, 6]).await?;
        if let Ok(length) = stream.read_u32().await {
            assert_eq!(length, 1);
            assert_eq!(stream.read_u8().await?, 11);
            stream.write_all(&[0, 0, 0, 5, 12, 0, 0, 0, 0]).await?;
        }
        Ok(())
    }

    async fn fixture_agent_probe(handle: server::Handle) -> &'static [u8] {
        let Ok(channel) = handle.channel_open_agent().await else {
            return b"blocked";
        };
        let mut agent = channel.into_stream();
        let mut reply = [0; 9];
        if agent.write_all(&[0, 0, 0, 1, 11]).await.is_ok()
            && agent.read_exact(&mut reply).await.is_ok()
            && reply == [0, 0, 0, 5, 12, 0, 0, 0, 0]
        {
            b"relayed"
        } else {
            b"relay-failed"
        }
    }

    fn fixture_key(seed: u8) -> Result<PrivateKey> {
        let key = Ed25519Keypair::from_seed(&[seed; 32]);
        Ok(PrivateKey::new(KeypairData::Ed25519(key), "capterm-test")?)
    }

    async fn start_fixture(
        seed: u8,
        client_key: PublicKey,
    ) -> Result<(u16, Arc<RwLock<PrivateKey>>, JoinHandle<()>)> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let key = Arc::new(RwLock::new(fixture_key(seed)?));
        let server_key = key.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let config = Arc::new(server::Config {
                    keys: vec![server_key.read().await.clone()],
                    auth_rejection_time: Duration::ZERO,
                    ..Default::default()
                });
                let handler = Fixture {
                    client_key: client_key.clone(),
                    shell: None,
                    channels: HashMap::new(),
                    sftp_rejected: false,
                    sftp_files: Arc::new(std::sync::Mutex::new(HashMap::from([
                        (
                            FIXTURE_PATH.to_owned(),
                            (FIXTURE_TEXT.as_bytes().to_vec(), fixture_file_attrs()),
                        ),
                        (
                            FIXTURE_LARGE_PATH.to_owned(),
                            (
                                vec![b'x'; crate::file_text::PREVIEW_BYTES as usize + 1],
                                FileAttributes {
                                    size: Some(FIXTURE_LARGE_SIZE),
                                    ..fixture_file_attrs()
                                },
                            ),
                        ),
                    ]))),
                    forwards: HashMap::new(),
                };
                tokio::spawn(async move {
                    if let Ok(session) = server::run_stream(config, stream, handler).await {
                        let _ = session.await;
                    }
                });
            }
        });
        Ok((port, key, task))
    }

    struct Fixture {
        client_key: PublicKey,
        shell: Option<ChannelId>,
        channels: HashMap<ChannelId, Channel<Msg>>,
        sftp_rejected: bool,
        sftp_files: FixtureFileStore,
        forwards: HashMap<(String, u32), JoinHandle<()>>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for task in self.forwards.values() {
                task.abort();
            }
        }
    }

    impl server::Handler for Fixture {
        type Error = anyhow::Error;

        async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth> {
            Ok(if user == "fixture" && password == "fixture-password" {
                Auth::Accept
            } else {
                Auth::reject()
            })
        }

        async fn auth_publickey(&mut self, user: &str, key: VerifiedPublicKey<'_>) -> Result<Auth> {
            Ok(
                if user == "fixture" && key.public_key.key_data() == self.client_key.key_data() {
                    Auth::Accept
                } else {
                    Auth::reject()
                },
            )
        }

        async fn channel_open_session(
            &mut self,
            channel: Channel<Msg>,
            reply: ChannelOpenHandle,
            _session: &mut Session,
        ) -> Result<()> {
            self.channels.insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }

        async fn channel_open_direct_tcpip(
            &mut self,
            channel: Channel<Msg>,
            host: &str,
            port: u32,
            _originator: &str,
            _originator_port: u32,
            reply: ChannelOpenHandle,
            _session: &mut Session,
        ) -> Result<()> {
            assert_eq!(host, "127.0.0.1");
            let mut tcp = TcpStream::connect((host, u16::try_from(port)?)).await?;
            reply.accept().await;
            tokio::spawn(async move {
                let mut channel = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut channel, &mut tcp).await;
            });
            Ok(())
        }

        async fn tcpip_forward(
            &mut self,
            address: &str,
            port: &mut u32,
            session: &mut Session,
        ) -> Result<bool> {
            if address != "127.0.0.1" {
                return Ok(false);
            }
            let listener = TcpListener::bind((address, u16::try_from(*port)?)).await?;
            let bound = u32::from(listener.local_addr()?.port());
            *port = bound;
            let session = session.handle();
            let address = address.to_owned();
            let bound_address = address.clone();
            let task = tokio::spawn(async move {
                while let Ok((mut socket, source)) = listener.accept().await {
                    let session = session.clone();
                    let address = bound_address.clone();
                    tokio::spawn(async move {
                        if let Ok(channel) = session
                            .channel_open_forwarded_tcpip(
                                address,
                                bound,
                                source.ip().to_string(),
                                u32::from(source.port()),
                            )
                            .await
                        {
                            let mut channel = channel.into_stream();
                            let _ = tokio::io::copy_bidirectional(&mut channel, &mut socket).await;
                        }
                    });
                }
            });
            self.forwards.insert((address, bound), task);
            Ok(true)
        }

        async fn cancel_tcpip_forward(
            &mut self,
            address: &str,
            port: u32,
            _session: &mut Session,
        ) -> Result<bool> {
            if let Some(task) = self.forwards.remove(&(address.to_owned(), port)) {
                task.abort();
                Ok(true)
            } else {
                Ok(false)
            }
        }

        async fn pty_request(
            &mut self,
            channel: ChannelId,
            term: &str,
            columns: u32,
            rows: u32,
            _width: u32,
            _height: u32,
            _modes: &[(Pty, u32)],
            session: &mut Session,
        ) -> Result<()> {
            assert_eq!((term, columns, rows), ("xterm-256color", 80, 24));
            session.channel_success(channel)?;
            Ok(())
        }

        async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<()> {
            self.channels.remove(&channel);
            self.shell = Some(channel);
            session.channel_success(channel)?;
            Ok(())
        }

        async fn agent_request(
            &mut self,
            _channel: ChannelId,
            _session: &mut Session,
        ) -> Result<bool> {
            Ok(true)
        }

        async fn subsystem_request(
            &mut self,
            channel: ChannelId,
            name: &str,
            session: &mut Session,
        ) -> Result<()> {
            assert_eq!(name, "sftp");
            let stream = self
                .channels
                .remove(&channel)
                .context("missing SFTP channel")?;
            if self.sftp_rejected {
                session.channel_success(channel)?;
                russh_sftp::server::run(
                    stream.into_stream(),
                    FixtureFiles {
                        sent_directory: false,
                        files: self.sftp_files.clone(),
                    },
                )
                .await;
            } else {
                self.sftp_rejected = true;
                session.channel_failure(channel)?;
            }
            Ok(())
        }

        async fn window_change_request(
            &mut self,
            channel: ChannelId,
            columns: u32,
            rows: u32,
            _width: u32,
            _height: u32,
            session: &mut Session,
        ) -> Result<()> {
            session.data(channel, format!("{columns}x{rows}").into_bytes())?;
            Ok(())
        }

        async fn data(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<()> {
            if self.shell == Some(channel) {
                session.data(channel, data.to_vec())?;
            }
            Ok(())
        }

        async fn exec_request(
            &mut self,
            channel: ChannelId,
            command: &[u8],
            session: &mut Session,
        ) -> Result<()> {
            self.channels.remove(&channel);
            session.channel_success(channel)?;
            if command == b"fixture-agent" {
                let handle = session.handle();
                tokio::spawn(async move {
                    let probe =
                        timeout(Duration::from_secs(4), fixture_agent_probe(handle.clone())).await;
                    let message = probe.unwrap_or(b"relay-timeout");
                    let _ = handle.data(channel, message.to_vec()).await;
                    let _ = handle.exit_status_request(channel, 0).await;
                    let _ = handle.eof(channel).await;
                    let _ = handle.close(channel).await;
                });
                return Ok(());
            }
            assert_eq!(command, METRIC_COMMAND.as_bytes());
            session.data(channel, METRIC_SAMPLE.to_vec())?;
            session.exit_status_request(channel, 0)?;
            session.eof(channel)?;
            session.close(channel)?;
            Ok(())
        }
    }

    type FixtureFileStore = Arc<std::sync::Mutex<HashMap<String, (Vec<u8>, FileAttributes)>>>;

    struct FixtureFiles {
        sent_directory: bool,
        files: FixtureFileStore,
    }

    impl russh_sftp::server::Handler for FixtureFiles {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn init(
            &mut self,
            _: u32,
            _: HashMap<String, String>,
        ) -> Result<Version, Self::Error> {
            let mut version = Version::new();
            version
                .extensions
                .insert(POSIX_RENAME.to_owned(), "1".to_owned());
            Ok(version)
        }

        async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
            assert!(matches!(path.as_str(), "." | "/fixture"));
            Ok(Name {
                id,
                files: vec![SftpFile::dummy("/fixture")],
            })
        }

        async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
            assert_eq!(path, "/fixture");
            self.sent_directory = false;
            Ok(Handle { id, handle: path })
        }

        async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
            assert_eq!(handle, "/fixture");
            if std::mem::replace(&mut self.sent_directory, true) {
                return Err(StatusCode::Eof);
            }
            Ok(Name {
                id,
                files: vec![
                    SftpFile::new("notes.txt", fixture_file_attrs()),
                    SftpFile::new(
                        "config",
                        FileAttributes {
                            permissions: Some(0o040755),
                            ..Default::default()
                        },
                    ),
                ],
            })
        }

        async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            let files = self.files.lock().unwrap();
            let (_, attrs) = files.get(&path).ok_or(StatusCode::NoSuchFile)?;
            Ok(Attrs {
                id,
                attrs: attrs.clone(),
            })
        }

        async fn open(
            &mut self,
            id: u32,
            path: String,
            flags: OpenFlags,
            attrs: FileAttributes,
        ) -> Result<Handle, Self::Error> {
            let mut files = self.files.lock().unwrap();
            if flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUDE) {
                if files.contains_key(&path) {
                    return Err(StatusCode::Failure);
                }
                let mut metadata = fixture_file_attrs();
                metadata.permissions = Some(0o100000 | attrs.permissions.unwrap_or(0o600));
                metadata.gid = Some(1000);
                metadata.size = Some(0);
                metadata.mtime = Some(FIXTURE_MTIME + 1);
                files.insert(path.clone(), (Vec::new(), metadata));
            } else if !files.contains_key(&path) {
                return Err(StatusCode::NoSuchFile);
            }
            Ok(Handle { id, handle: path })
        }

        async fn read(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, Self::Error> {
            let files = self.files.lock().unwrap();
            let (contents, _) = files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
            let offset = usize::try_from(offset).map_err(|_| StatusCode::Eof)?;
            let bytes = contents.get(offset..).ok_or(StatusCode::Eof)?;
            if bytes.is_empty() {
                return Err(StatusCode::Eof);
            }
            Ok(Data {
                id,
                data: bytes[..bytes.len().min(len as usize)].to_vec(),
            })
        }

        async fn write(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            data: Vec<u8>,
        ) -> Result<Status, Self::Error> {
            let mut files = self.files.lock().unwrap();
            let (contents, attrs) = files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
            assert_eq!(contents.len() as u64, offset);
            contents.extend(data);
            attrs.size = Some(contents.len() as u64);
            Ok(fixture_ok(id))
        }

        async fn setstat(
            &mut self,
            id: u32,
            path: String,
            attrs: FileAttributes,
        ) -> Result<Status, Self::Error> {
            let mut files = self.files.lock().unwrap();
            let (_, stored) = files.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
            stored.uid = attrs.uid.or(stored.uid);
            stored.gid = attrs.gid.or(stored.gid);
            stored.permissions = attrs
                .permissions
                .map(|permissions| 0o100000 | permissions)
                .or(stored.permissions);
            stored.atime = attrs.atime.or(stored.atime);
            stored.mtime = attrs.mtime.or(stored.mtime);
            Ok(fixture_ok(id))
        }

        async fn rename(
            &mut self,
            id: u32,
            old: String,
            new: String,
        ) -> Result<Status, Self::Error> {
            let mut files = self.files.lock().unwrap();
            if files.contains_key(&new) {
                return Err(StatusCode::Failure);
            }
            let source = files.remove(&old).ok_or(StatusCode::NoSuchFile)?;
            files.insert(new, source);
            Ok(fixture_ok(id))
        }

        async fn remove(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
            self.files
                .lock()
                .unwrap()
                .remove(&path)
                .ok_or(StatusCode::NoSuchFile)?;
            Ok(fixture_ok(id))
        }

        async fn extended(
            &mut self,
            id: u32,
            request: String,
            data: Vec<u8>,
        ) -> Result<Packet, Self::Error> {
            if request != POSIX_RENAME {
                return Err(StatusCode::OpUnsupported);
            }
            let (old, data) = fixture_sftp_string(&data).ok_or(StatusCode::BadMessage)?;
            let (new, _) = fixture_sftp_string(data).ok_or(StatusCode::BadMessage)?;
            let mut files = self.files.lock().unwrap();
            let source = files.remove(old).ok_or(StatusCode::NoSuchFile)?;
            files.insert(new.to_owned(), source);
            Ok(Packet::Status(fixture_ok(id)))
        }

        async fn close(&mut self, id: u32, _handle: String) -> Result<Status, Self::Error> {
            Ok(fixture_ok(id))
        }
    }

    fn fixture_sftp_string(bytes: &[u8]) -> Option<(&str, &[u8])> {
        let (length, bytes) = bytes.split_first_chunk::<4>()?;
        let (text, rest) = bytes.split_at_checked(u32::from_be_bytes(*length) as usize)?;
        Some((std::str::from_utf8(text).ok()?, rest))
    }

    fn fixture_ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        }
    }

    fn fixture_file_attrs() -> FileAttributes {
        FileAttributes {
            permissions: Some(0o100640),
            uid: Some(1000),
            gid: Some(1001),
            size: Some(FIXTURE_TEXT.len() as u64),
            atime: Some(FIXTURE_MTIME),
            mtime: Some(FIXTURE_MTIME),
            ..Default::default()
        }
    }
}
