use std::{
    collections::HashSet,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::Arc,
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
    fs::{self, File},
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{Mutex, OnceCell, mpsc, oneshot},
    time::timeout,
};

use crate::config::{ConnectionProfile, Hop};

const NETWORK_TIMEOUT: Duration = Duration::from_secs(15);
const TRUST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROBE_BYTES: usize = 64 * 1024;
const POSIX_RENAME: &str = "posix-rename@openssh.com";
static HOST_KEY_STORE: Mutex<()> = Mutex::const_new(());

pub struct TrustRequest {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub reply: oneshot::Sender<bool>,
}

#[derive(Clone, Debug)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub cpu: Option<f64>,
    pub memory: Option<f64>,
    pub disk: Option<f64>,
    pub load: Option<f64>,
    pub uptime_secs: Option<u64>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub rtt_ms: Option<u64>,
    pub os: Option<String>,
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
    trust: mpsc::UnboundedSender<TrustRequest>,
}

impl client::Handler for HostVerifier {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, identity: &PublicKeyOrCertificate) -> Result<bool> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = identity else {
            bail!("Host certificates require a configured CA");
        };
        if self.is_known(key).await? {
            return Ok(true);
        }

        let (reply, answer) = oneshot::channel();
        self.trust
            .send(TrustRequest {
                host: self.host.clone(),
                port: self.port,
                fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Host key confirmation is unavailable"))?;
        ensure!(
            timeout(TRUST_TIMEOUT, answer)
                .await
                .context("Host key confirmation timed out")?
                .unwrap_or(false),
            "Host key was declined"
        );

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
        if !self.check_pin(key)? {
            self.write_pin(key, parent)?;
        }
        Ok(true)
    }
}

impl HostVerifier {
    async fn is_known(&self, key: &PublicKey) -> Result<bool> {
        let _guard = HOST_KEY_STORE.lock().await;
        self.check_pin(key)
    }

    fn check_pin(&self, key: &PublicKey) -> Result<bool> {
        // Check readability explicitly. The russh helper treats an open error as an empty file.
        match std::fs::File::open(&self.known_hosts) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error).context("Reading CapTerm trusted hosts"),
        }
        let known = known_host_keys_path(&self.host, self.port, &self.known_hosts)
            .context("Reading CapTerm trusted hosts")?;
        ensure!(
            known.is_empty()
                || known
                    .iter()
                    .any(|(_, saved)| saved.key_data() == key.key_data()),
            "Host key changed for {}:{} (received {})",
            self.host,
            self.port,
            key.fingerprint(HashAlg::Sha256)
        );
        Ok(!known.is_empty())
    }

    fn write_pin(&self, key: &PublicKey, parent: &Path) -> Result<()> {
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

pub struct Connection {
    target: client::Handle<HostVerifier>,
    jumps: Vec<client::Handle<HostVerifier>>,
    sftp: OnceCell<Arc<SftpSession>>,
    edit_lock: Mutex<()>,
    cpu_sample: Mutex<Option<(u64, u64)>>,
}

impl Connection {
    pub async fn connect(
        profile: &ConnectionProfile,
        password: Option<String>,
        known_hosts: PathBuf,
        trust: mpsc::UnboundedSender<TrustRequest>,
    ) -> Result<Self> {
        let target = Hop {
            host: profile.host.clone(),
            user: profile.user.clone(),
            port: profile.port,
            identity_files: profile.identity_files.clone(),
        };
        let mut sessions: Vec<client::Handle<HostVerifier>> = Vec::new();
        let config = Arc::new(client::Config {
            keepalive_interval: Some(Duration::from_secs(25)),
            ..Default::default()
        });
        for (index, hop) in profile
            .jumps
            .iter()
            .chain(std::iter::once(&target))
            .enumerate()
        {
            let host = normalize_host(&hop.host)?;
            ensure!(
                hop.port != 0 && !hop.user.is_empty(),
                "SSH user and port are required"
            );
            let is_target = index == profile.jumps.len();
            let role = if is_target {
                "Target".to_string()
            } else {
                format!("Jump {}", index + 1)
            };
            let endpoint = format!("{role} {}@{}:{}", hop.user, host, hop.port);
            let verifier = HostVerifier {
                host: host.clone(),
                port: hop.port,
                known_hosts: known_hosts.clone(),
                trust: trust.clone(),
            };
            let mut session = match sessions.last() {
                Some(previous) => {
                    let stream = timeout(
                        NETWORK_TIMEOUT,
                        previous.channel_open_direct_tcpip(
                            &host,
                            u32::from(hop.port),
                            "127.0.0.1",
                            0,
                        ),
                    )
                    .await
                    .with_context(|| format!("{endpoint} forwarding timed out"))?
                    .with_context(|| format!("{endpoint} forwarding failed"))?
                    .into_stream();
                    timeout(
                        TRUST_TIMEOUT + NETWORK_TIMEOUT,
                        client::connect_stream(config.clone(), stream, verifier),
                    )
                    .await
                }
                None => {
                    let stream = timeout(
                        NETWORK_TIMEOUT,
                        TcpStream::connect((host.as_str(), hop.port)),
                    )
                    .await
                    .with_context(|| format!("{endpoint} TCP connection timed out"))?
                    .with_context(|| format!("{endpoint} TCP connection failed"))?;
                    stream.set_nodelay(true)?;
                    timeout(
                        TRUST_TIMEOUT + NETWORK_TIMEOUT,
                        client::connect_stream(config.clone(), stream, verifier),
                    )
                    .await
                }
            }
            .with_context(|| format!("{endpoint} SSH handshake timed out"))?
            .with_context(|| format!("{endpoint} SSH handshake failed"))?;
            let password = if is_target { password.as_deref() } else { None };
            timeout(NETWORK_TIMEOUT, authenticate(&mut session, hop, password))
                .await
                .with_context(|| format!("{endpoint} authentication timed out"))?
                .with_context(|| format!("{endpoint} authentication failed"))?;
            if is_target {
                return Ok(Self {
                    target: session,
                    jumps: sessions,
                    sftp: OnceCell::new(),
                    edit_lock: Mutex::new(()),
                    cpu_sample: Mutex::new(None),
                });
            }
            sessions.push(session);
        }
        unreachable!("connection route always includes the target")
    }

    pub async fn disconnect(&self) -> Result<()> {
        let target_result = timeout(
            NETWORK_TIMEOUT,
            self.target
                .disconnect(Disconnect::ByApplication, "Disconnected", ""),
        )
        .await;
        for hop in self.jumps.iter().rev() {
            let _ = timeout(
                NETWORK_TIMEOUT,
                hop.disconnect(Disconnect::ByApplication, "Disconnected", ""),
            )
            .await;
        }
        target_result.context("Disconnect timed out")??;
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
                    "xterm-256color",
                    u32::from(columns.max(1)),
                    u32::from(rows.max(1)),
                    0,
                    0,
                    &[],
                )
                .await?;
            wait_for_success(&mut channel, &mut initial, "PTY").await?;
            channel.request_shell(true).await?;
            wait_for_success(&mut channel, &mut initial, "shell").await
        })
        .await
        .context("Opening remote terminal timed out")
        .and_then(|result| result);
        if let Err(error) = setup {
            close_channel(&channel).await;
            return Err(error);
        }
        let (commands, command_rx) = mpsc::channel(128);
        let (event_tx, events) = mpsc::channel(128);
        tokio::spawn(run_shell(channel, command_rx, event_tx, initial));
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
                    name,
                    is_dir: entry.file_type().is_dir(),
                    size: metadata.size,
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

    pub async fn read_text(&self, path: &str) -> Result<String> {
        let sftp = self.sftp().await?;
        read_remote_text(&sftp, path).await
    }

    pub async fn save_text(&self, path: &str, original: &str, edited: &str) -> Result<()> {
        let _guard = self.edit_lock.lock().await;
        let sftp = self.sftp().await?;
        let metadata = sftp.symlink_metadata(path).await?;
        ensure!(
            metadata.file_type().is_file(),
            "Only regular files can be edited"
        );
        let edited = preserve_line_endings(original, edited);
        ensure!(
            edited.len() <= MAX_TEXT_BYTES,
            "Text file exceeds the 2 MiB editor limit"
        );

        let raw = RawSftpSession::new(self.sftp_channel().await?.into_stream());
        let result = async {
            let version = raw.init().await?;
            ensure!(
                version
                    .extensions
                    .get(POSIX_RENAME)
                    .is_some_and(|version| version == "1"),
                "The server does not support safe replacement of an existing file"
            );
            let temp = remote_temp(path);
            let mut staged = RemoteStage::create(sftp.clone(), temp).await?;
            staged
                .file()
                .write_all(edited.as_bytes())
                .await
                .context("Writing temporary remote file")?;
            staged
                .finish_writing()
                .await
                .context("Closing temporary remote file")?;

            let owner = sftp.symlink_metadata(&staged.path).await?;
            let (uid, gid) = if (metadata.uid, metadata.gid) == (owner.uid, owner.gid) {
                (None, None)
            } else {
                (metadata.uid, metadata.gid)
            };
            sftp.set_metadata(
                &staged.path,
                FileAttributes {
                    uid,
                    gid,
                    permissions: metadata.permissions.map(|mode| mode & 0o7777),
                    ..FileAttributes::empty()
                },
            )
            .await
            .context("Preserving remote file permissions and ownership")?;

            let current = sftp.symlink_metadata(path).await?;
            ensure!(
                current.file_type().is_file()
                    && current.size == metadata.size
                    && current.mtime == metadata.mtime
                    && current.permissions == metadata.permissions
                    && current.uid == metadata.uid
                    && current.gid == metadata.gid
                    && read_remote_text(&sftp, path).await? == original,
                "The remote file changed. Reopen it before saving your edits."
            );
            posix_rename(&raw, &staged.path, path).await?;
            staged.commit();
            Ok(())
        }
        .await;
        let _ = raw.close_session();
        result
    }

    pub async fn upload(&self, local: &Path, remote_dir: &str) -> Result<()> {
        let name = local
            .file_name()
            .and_then(|name| name.to_str())
            .context("A UTF-8 file name is required")?;
        ensure!(valid_remote_name(name), "Invalid remote file name");
        let mut source = File::open(local)
            .await
            .context("Opening local upload source")?;
        ensure!(
            source.metadata().await?.is_file(),
            "Only regular files can be uploaded"
        );
        let sftp = self.sftp().await?;
        let remote_dir = sftp.canonicalize(remote_dir).await?;
        let destination = join_remote(&remote_dir, name);
        ensure!(
            !sftp.try_exists(&destination).await?,
            "A remote file named {name} already exists"
        );
        let mut staged = RemoteStage::create(sftp.clone(), remote_temp(&destination)).await?;
        io::copy(&mut source, staged.file())
            .await
            .context("Uploading file")?;
        staged
            .finish_writing()
            .await
            .context("Closing uploaded file")?;
        sftp.rename(&staged.path, &destination)
            .await
            .context("Finishing upload")?;
        staged.commit();
        Ok(())
    }

    pub async fn download(&self, remote: &str, local: &Path) -> Result<()> {
        ensure!(
            remote.rsplit('/').next().is_some_and(valid_local_name),
            "The remote file name cannot be used safely on this platform"
        );
        let filename = local
            .file_name()
            .and_then(|name| name.to_str())
            .context("A UTF-8 destination name is required")?;
        ensure!(valid_local_name(filename), "Invalid local destination name");
        let parent = local
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let sftp = self.sftp().await?;
        ensure!(
            sftp.symlink_metadata(remote).await?.file_type().is_file(),
            "Only regular remote files can be downloaded"
        );
        let mut source = sftp.open(remote).await.context("Opening remote file")?;
        let temp = tempfile::Builder::new()
            .prefix(".capterm-")
            .tempfile_in(parent)
            .context("Creating temporary download")?;
        let (file, temp_path) = temp.into_parts();
        let mut file = File::from_std(file);
        let copied = io::copy(&mut source, &mut file).await;
        let closed = source.close().await;
        copied.context("Downloading file")?;
        closed.context("Closing remote file")?;
        file.sync_all().await?;
        drop(file.into_std().await);
        temp_path
            .persist_noclobber(local)
            .context("Finishing download; the destination must be new")?;
        Ok(())
    }

    pub async fn metrics(&self) -> Result<Metrics> {
        let start = Instant::now();
        let ping = async {
            timeout(NETWORK_TIMEOUT, self.target.send_ping())
                .await
                .ok()?
                .ok()?;
            if self.target.is_closed() {
                return None;
            }
            Some(start.elapsed().as_millis().try_into().unwrap_or(u64::MAX))
        };
        let (probe, rtt_ms) = tokio::join!(self.run_metric_probe(), ping);
        let mut metrics = Metrics {
            rtt_ms,
            ..Metrics::default()
        };
        let output = match probe {
            Ok(output) => output,
            Err(_) if rtt_ms.is_some() => return Ok(metrics),
            Err(error) => return Err(error),
        };
        let mut cpu_sample = self.cpu_sample.lock().await;
        parse_metrics(&output, &mut metrics, &mut cpu_sample);
        Ok(metrics)
    }

    async fn run_metric_probe(&self) -> Result<Vec<u8>> {
        let mut channel = self.channel().await?;
        let result = timeout(NETWORK_TIMEOUT, async {
            channel.exec(true, METRIC_COMMAND).await?;
            let mut output = Vec::new();
            wait_for_success(&mut channel, &mut output, "metric probe").await?;
            let mut exit = None;
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => extend_probe(&mut output, &data)?,
                    ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            ensure!(exit == Some(0), "Remote metric probe was unavailable");
            Ok(output)
        })
        .await
        .context("Remote metric probe timed out");
        close_channel(&channel).await;
        result?
    }
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

async fn authenticate(
    session: &mut client::Handle<HostVerifier>,
    hop: &Hop,
    password: Option<&str>,
) -> Result<()> {
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
            return Ok(());
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
                    return Ok(());
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

    #[cfg(windows)]
    let agent = timeout(
        Duration::from_secs(1),
        AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent"),
    )
    .await;
    #[cfg(unix)]
    let agent = timeout(Duration::from_secs(1), AgentClient::connect_env()).await;
    #[cfg(any(windows, unix))]
    if !accepted_key && let Ok(Ok(agent)) = agent {
        match authenticate_agent(session, user, hash, agent, &mut tried).await {
            Some(client::AuthResult::Success) => return Ok(()),
            Some(client::AuthResult::Failure {
                partial_success, ..
            }) => accepted_key = partial_success,
            None => {}
        }
    }

    if let Some(password) = password {
        if (!password_tried || accepted_key)
            && session
                .authenticate_password(user, password)
                .await?
                .success()
        {
            return Ok(());
        }
        let result = session
            .authenticate_keyboard_interactive_start(user, None::<String>)
            .await?;
        if matches!(result, client::KeyboardInteractiveAuthResponse::Success) {
            return Ok(());
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
            return Ok(());
        }
    }

    match last_key_error {
        Some(error) => bail!(
            "Server rejected available credentials; key could not be loaded ({error}). Encrypted keys can be loaded into an SSH agent."
        ),
        None => bail!("Server rejected available credentials"),
    }
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
                if !tried.insert(key.fingerprint(HashAlg::Sha256).to_string()) {
                    continue;
                }
                session
                    .authenticate_publickey_with(user, key, hash, &mut agent)
                    .await
            }
            AgentIdentity::Certificate { certificate, .. } => {
                let fingerprint = certificate.public_key().fingerprint(HashAlg::Sha256);
                if !tried.insert(format!("cert:{fingerprint}")) {
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
) {
    let (mut read, write) = channel.split();
    if !initial.is_empty() && events.send(ShellEvent::Output(initial)).await.is_err() {
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

async fn read_remote_text(sftp: &SftpSession, path: &str) -> Result<String> {
    let metadata = sftp.symlink_metadata(path).await?;
    ensure!(
        metadata.file_type().is_file(),
        "Only regular files can be edited"
    );
    ensure!(
        metadata.size.unwrap_or(0) <= MAX_TEXT_BYTES as u64,
        "Text file exceeds the 2 MiB editor limit"
    );
    let mut file = sftp.open(path).await?;
    let mut data = Vec::new();
    let read = (&mut file)
        .take(MAX_TEXT_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .await;
    let closed = file.close().await;
    read?;
    closed?;
    ensure!(
        data.len() <= MAX_TEXT_BYTES,
        "Text file exceeds the 2 MiB editor limit"
    );
    ensure!(
        !data.contains(&0),
        "The selected file appears to contain binary data"
    );
    String::from_utf8(data).context("The editor only supports UTF-8 files")
}

fn preserve_line_endings(original: &str, edited: &str) -> String {
    let original_newlines = original.bytes().filter(|&byte| byte == b'\n').count();
    if original_newlines > 0 && original.matches("\r\n").count() == original_newlines {
        edited.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        edited.to_owned()
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

const METRIC_COMMAND: &str = r#"
LC_ALL=C; export LC_ALL
printf 'os '; uname -s
if [ -r /etc/os-release ]; then sed -n 's/^PRETTY_NAME=/os /p' /etc/os-release; fi
if [ -r /proc/stat ]; then awk '/^cpu / {printf "cpu %.0f %.0f\n", $2+$3+$4+$5+$6+$7+$8+$9, $5+$6; exit}' /proc/stat; fi
if [ -r /proc/meminfo ]; then awk '/^MemTotal:/ {t=$2} /^MemAvailable:/ {a=$2} END {if (t && a != "") printf "memory %.0f %.0f\n", t, a}' /proc/meminfo; fi
if [ -r /proc/loadavg ]; then awk '{print "load", $1}' /proc/loadavg; fi
if [ -r /proc/uptime ]; then awk '{printf "uptime %.0f\n", int($1)}' /proc/uptime; fi
df -Pk / 2>/dev/null | awk 'NR==2 {print "disk", $5}'
"#;

fn parse_metrics(output: &[u8], metrics: &mut Metrics, cpu_sample: &mut Option<(u64, u64)>) {
    let output = String::from_utf8_lossy(output);
    for line in output.lines() {
        let Some((name, value)) = line.split_once(' ') else {
            continue;
        };
        match name {
            "os" => {
                let value = value.trim().trim_matches(['\'', '"']);
                let label: String = value
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(80)
                    .collect();
                if !label.is_empty() {
                    metrics.os = Some(label);
                }
            }
            "cpu" => {
                if let Some((total, idle)) = parse_pair(value) {
                    let previous = cpu_sample.replace((total, idle));
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
            "memory" => {
                if let Some((total, available)) = parse_pair(value)
                    && total > 0
                    && available <= total
                {
                    let used = total - available;
                    metrics.memory_used_bytes = used.checked_mul(1024);
                    metrics.memory_total_bytes = total.checked_mul(1024);
                    metrics.memory = Some(used as f64 * 100.0 / total as f64);
                }
            }
            "disk" => {
                metrics.disk = value
                    .trim_end_matches('%')
                    .parse::<f64>()
                    .ok()
                    .filter(|value| (0.0..=100.0).contains(value))
            }
            "load" => {
                metrics.load = value
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite() && *value >= 0.0)
            }
            "uptime" => metrics.uptime_secs = value.parse().ok(),
            _ => {}
        }
    }
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
    use tokio::{net::TcpListener, sync::RwLock, task::JoinHandle};

    const METRIC_SAMPLE: &[u8] = b"os Test Linux\ncpu 1000000000000 500000000000\nmemory 1024 256\nload 0.42\nuptime 3600\ndisk 68%\n";

    #[test]
    fn metrics_use_deltas_and_report_partial_results() {
        let mut counters = None;
        let mut metrics = Metrics::default();
        parse_metrics(METRIC_SAMPLE, &mut metrics, &mut counters);
        assert_eq!(metrics.cpu, None);
        assert_eq!(metrics.memory, Some(75.0));
        assert_eq!(metrics.memory_total_bytes, Some(1_048_576));
        assert_eq!(metrics.memory_used_bytes, Some(786_432));
        assert_eq!(metrics.disk, Some(68.0));
        assert_eq!(metrics.os.as_deref(), Some("Test Linux"));

        let mut metrics = Metrics::default();
        parse_metrics(
            b"cpu 1000000000100 500000000020\nload NaN\n",
            &mut metrics,
            &mut counters,
        );
        assert_eq!(metrics.cpu, Some(80.0));
        assert_eq!(metrics.load, None);

        let mut metrics = Metrics::default();
        parse_metrics(b"cpu 20 10\n", &mut metrics, &mut counters);
        assert_eq!(metrics.cpu, None);
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
    async fn connects_through_two_jumps_and_rejects_a_changed_key() -> Result<()> {
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
                })
                .collect(),
            source: None,
        };
        let (trust, mut requests) = mpsc::unbounded_channel::<TrustRequest>();
        let approve = tokio::spawn(async move {
            for port in [jump_a, jump_b, target] {
                let request = requests.recv().await.expect("host key prompt");
                assert_eq!(request.port, port);
                assert!(request.fingerprint.starts_with("SHA256:"));
                request.reply.send(true).unwrap();
            }
            requests
        });
        let connect = Connection::connect(
            &profile,
            Some("fixture-password".into()),
            known_hosts.clone(),
            trust.clone(),
        );
        let connection = timeout(Duration::from_secs(10), connect).await??;
        let mut requests = approve.await?;
        let (commands, mut output) = connection.shell(80, 24).await?;
        let error = connection
            .list(".")
            .await
            .expect_err("fixture has no SFTP subsystem");
        assert!(error.to_string().contains("SFTP subsystem"));
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
        assert_eq!(metrics.memory, Some(75.0));
        assert!(metrics.rtt_ms.is_some());
        connection.disconnect().await?;

        *target_key.write().await = fixture_key(23)?;
        let reconnect = Connection::connect(
            &profile,
            Some("fixture-password".into()),
            known_hosts,
            trust,
        );
        let error = match timeout(Duration::from_secs(10), reconnect).await? {
            Ok(_) => bail!("Changed host key was accepted"),
            Err(error) => error,
        };
        assert!(format!("{error:#}").contains("Host key changed"));
        assert!(requests.try_recv().is_err());
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
            _channel: Channel<Msg>,
            reply: ChannelOpenHandle,
            _session: &mut Session,
        ) -> Result<()> {
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
                let _ = io::copy_bidirectional(&mut channel, &mut tcp).await;
            });
            Ok(())
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
            self.shell = Some(channel);
            session.channel_success(channel)?;
            Ok(())
        }

        async fn subsystem_request(
            &mut self,
            channel: ChannelId,
            name: &str,
            session: &mut Session,
        ) -> Result<()> {
            assert_eq!(name, "sftp");
            session.channel_failure(channel)?;
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
            assert_eq!(command, METRIC_COMMAND.as_bytes());
            session.channel_success(channel)?;
            session.data(channel, METRIC_SAMPLE.to_vec())?;
            session.exit_status_request(channel, 0)?;
            session.eof(channel)?;
            session.close(channel)?;
            Ok(())
        }
    }
}
