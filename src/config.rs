use std::{
    collections::{BTreeMap, BTreeSet, HashSet, VecDeque},
    env,
    fs::{self, File},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hop {
    pub host: String,
    pub user: String,
    pub port: u16,
    #[serde(default)]
    pub identity_files: Vec<String>,
    #[serde(default)]
    pub ssh: SshOptions,
    #[serde(default)]
    pub reference: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    #[serde(default)]
    pub auth: AuthMethod,
    #[serde(default)]
    pub identity_files: Vec<String>,
    #[serde(default)]
    pub jumps: Vec<Hop>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub ssh: SshOptions,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Password,
    // Preserve key and agent authentication for profiles saved before this setting existed.
    #[default]
    Key,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshOptions {
    pub identities_only: bool,
    pub forward_agent: bool,
    pub identity_agent: Option<String>,
    pub connect_timeout: Option<u32>,
    pub keepalive_interval: Option<u32>,
    pub compression: bool,
    pub term: Option<String>,
    pub set_env: BTreeMap<String, String>,
    pub send_env: Vec<String>,
    pub proxy_command: Option<Vec<String>>,
    pub proxy_command_alias: Option<String>,
    pub network_proxy: Option<NetworkProxy>,
    pub port_forwards: Vec<crate::ssh::PortForward>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyKind {
    Socks5,
    HttpConnect,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkProxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
}

impl NetworkProxy {
    pub fn parse(value: &str) -> Result<Self> {
        let (scheme, address) = value
            .split_once("://")
            .context("代理地址须包含 socks5:// 或 http://")?;
        let kind = match scheme.to_ascii_lowercase().as_str() {
            "socks5" => ProxyKind::Socks5,
            "http" => ProxyKind::HttpConnect,
            _ => bail!("仅支持 SOCKS5 和 HTTP CONNECT 代理"),
        };
        let (host, user, port) = parse_hop(address)?;
        ensure!(user.is_none(), "代理地址使用主机和端口");
        let port = port.unwrap_or(match kind {
            ProxyKind::Socks5 => 1080,
            ProxyKind::HttpConnect => 8080,
        });
        Ok(Self {
            kind,
            host: host.to_owned(),
            port,
        })
    }
}

impl std::fmt::Display for NetworkProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scheme = match self.kind {
            ProxyKind::Socks5 => "socks5",
            ProxyKind::HttpConnect => "http",
        };
        let host = &self.host;
        if host.contains(':') {
            write!(f, "{scheme}://[{host}]:{}", self.port)
        } else {
            write!(f, "{scheme}://{host}:{}", self.port)
        }
    }
}

impl ConnectionProfile {
    pub fn ssh_command(&self) -> String {
        if self.ssh.network_proxy.is_some()
            || self
                .jumps
                .iter()
                .any(|hop| hop.ssh.network_proxy.is_some() || hop.ssh.proxy_command.is_some())
        {
            return String::new();
        }
        let mut args = vec!["ssh".to_owned()];
        if !self.jumps.is_empty() {
            let jumps = self
                .jumps
                .iter()
                .map(|hop| ssh_address(&hop.user, &hop.host, Some(hop.port)))
                .collect::<Vec<_>>();
            args.extend(["-J".into(), quote_ssh(&jumps.join(","))]);
        }
        if let Some(command) = &self.ssh.proxy_command {
            if validate_proxy_command(command.clone())
                .ok()
                .flatten()
                .is_none()
                || [&self.host, &self.user, &self.name]
                    .into_iter()
                    .any(|value| {
                        !value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b".-_:".contains(&byte))
                    })
                || (cfg!(windows) && command.iter().any(|arg| arg.contains(['%', '"'])))
            {
                return String::new();
            }
            let command = if cfg!(windows) {
                format_argv(command)
            } else {
                command
                    .iter()
                    .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            args.extend(["-o".into(), quote_ssh(&format!("ProxyCommand={command}"))]);
        }
        for forward in &self.ssh.port_forwards {
            let (flag, rule) = forward.openssh_args();
            args.extend([flag.into(), quote_ssh(&rule)]);
        }
        for identity in &self.identity_files {
            if identity.eq_ignore_ascii_case("none") {
                args.extend(["-o".into(), "IdentityFile=none".into()]);
            } else {
                args.extend(["-i".into(), quote_ssh(identity)]);
            }
        }
        if self.ssh.compression {
            args.push("-C".into());
        }
        args.push(if self.ssh.forward_agent {
            "-A".into()
        } else {
            "-a".into()
        });
        for (key, value) in [
            (
                "IdentitiesOnly",
                self.ssh.identities_only.then(|| "yes".to_owned()),
            ),
            ("IdentityAgent", self.ssh.identity_agent.clone()),
            (
                "ConnectTimeout",
                self.ssh.connect_timeout.map(|value| value.to_string()),
            ),
            (
                "ServerAliveInterval",
                self.ssh.keepalive_interval.map(|value| value.to_string()),
            ),
        ] {
            if let Some(value) = value {
                args.extend(["-o".into(), quote_ssh(&format!("{key}={value}"))]);
            }
        }
        if self.port != 22 {
            args.extend(["-p".into(), self.port.to_string()]);
        }
        args.extend([
            "--".into(),
            quote_ssh(&ssh_address(&self.user, &self.host, None)),
        ]);
        args.join(" ")
    }
}

pub fn ssh_address(user: &str, host: &str, port: Option<u16>) -> String {
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    match port {
        Some(port) => format!("{user}@{host}:{port}"),
        None => format!("{user}@{host}"),
    }
}

fn quote_ssh(value: &str) -> String {
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-._/@:,[]".contains(character))
    {
        value.to_owned()
    } else {
        let escaped = if cfg!(windows) {
            value.replace('\'', "''")
        } else {
            value.replace('\'', "'\\''")
        };
        format!("'{escaped}'")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    Legacy,
    Dark,
    Light,
    System,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampPreference {
    #[default]
    Seconds,
    Milliseconds,
    Relative,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferConflictPreference {
    #[default]
    Ask,
    Overwrite,
    Skip,
    KeepBoth,
}

impl From<TimestampPreference> for crate::terminal::TimestampFormat {
    fn from(preference: TimestampPreference) -> Self {
        match preference {
            TimestampPreference::Seconds => Self::Seconds,
            TimestampPreference::Milliseconds => Self::Milliseconds,
            TimestampPreference::Relative => Self::Relative,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShiftEnterPreference {
    #[default]
    EscapeEnter,
    Enter,
    LineFeed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalNotificationPreference {
    Off,
    InApp,
    #[default]
    InAppAndSystem,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Osc52WritePreference {
    #[default]
    Ask,
    Allow,
    Deny,
}

pub const DEFAULT_TERMINAL_TYPE: &str = "xterm-256color";

pub fn validate_terminal_type(term: &str) -> Result<()> {
    ensure!(
        !term.chars().any(|ch| ch.is_whitespace() || ch.is_control()),
        "终端类型不能包含空格或控制字符。"
    );
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LinkedConfig {
    pub path: String,
    pub excluded: BTreeSet<String>,
    pub synced_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub profiles: Vec<ConnectionProfile>,
    pub config_path: String,
    pub light: bool,
    pub theme: ThemePreference,
    pub linux_highlighting: bool,
    pub show_line_numbers: bool,
    pub show_timestamps: bool,
    pub timestamp_format: TimestampPreference,
    pub dim_repeated_timestamps: bool,
    pub idle_threshold: u64,
    pub terminal_font_size: f32,
    pub terminal_line_height: f32,
    pub terminal_scrollback_lines: u64,
    pub ssh_terminal_type: Option<String>,
    pub kitty_keyboard: bool,
    pub synchronized_output: bool,
    pub shift_enter: ShiftEnterPreference,
    pub terminal_notifications: TerminalNotificationPreference,
    pub osc52_write: Osc52WritePreference,
    pub confirm_multiline_paste: bool,
    pub ui_font_family: Option<String>,
    pub mono_font_family: Option<String>,
    pub watch_ssh_configs: bool,
    pub linked_configs: Vec<LinkedConfig>,
    pub monitor_interval: u64,
    pub monitor_pause_in_background: bool,
    pub monitor_warn_pct: u8,
    pub monitor_danger_pct: u8,
    pub snippets: Vec<String>,
    pub sender_history: BTreeMap<String, VecDeque<String>>,
    pub shortcuts: crate::shortcuts::ShortcutBindings,
    pub ssh_keepalive: u32,
    pub ssh_timeout: u32,
    pub ssh_identity_agent: Option<String>,
    pub ssh_require_known_host: bool,
    pub reuse_jumps: bool,
    pub download_directory: String,
    pub transfer_concurrency: usize,
    pub transfer_conflict: TransferConflictPreference,
    pub transfer_preserve_metadata: bool,
    pub file_edit_limit_mb: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            config_path: "~/.ssh/config".into(),
            light: false,
            theme: ThemePreference::Legacy,
            linux_highlighting: true,
            show_line_numbers: true,
            show_timestamps: true,
            timestamp_format: TimestampPreference::Seconds,
            dim_repeated_timestamps: true,
            idle_threshold: 60,
            terminal_font_size: 13.,
            terminal_line_height: 20.,
            terminal_scrollback_lines: 10_000,
            ssh_terminal_type: None,
            kitty_keyboard: true,
            synchronized_output: true,
            shift_enter: ShiftEnterPreference::default(),
            terminal_notifications: TerminalNotificationPreference::default(),
            osc52_write: Osc52WritePreference::default(),
            confirm_multiline_paste: true,
            ui_font_family: None,
            mono_font_family: None,
            watch_ssh_configs: true,
            linked_configs: Vec::new(),
            monitor_interval: 5,
            monitor_pause_in_background: true,
            monitor_warn_pct: 80,
            monitor_danger_pct: 95,
            snippets: Vec::new(),
            sender_history: BTreeMap::new(),
            shortcuts: crate::shortcuts::ShortcutBindings::default(),
            ssh_keepalive: 30,
            ssh_timeout: 15,
            ssh_identity_agent: None,
            ssh_require_known_host: false,
            reuse_jumps: true,
            download_directory: String::new(),
            transfer_concurrency: 3,
            transfer_conflict: TransferConflictPreference::Ask,
            transfer_preserve_metadata: true,
            file_edit_limit_mb: 5,
        }
    }
}

pub const MAX_SCROLLBACK_LINES: u64 = 100_000;

impl Settings {
    pub fn scrollback_lines(&self) -> u64 {
        self.terminal_scrollback_lines.min(MAX_SCROLLBACK_LINES)
    }

    pub fn download_dir(&self) -> PathBuf {
        if self.download_directory.is_empty() {
            dirs::download_dir()
                .filter(|path| path.is_dir())
                .or_else(dirs::home_dir)
                .unwrap_or_else(std::env::temp_dir)
        } else {
            PathBuf::from(&self.download_directory)
        }
    }

    pub fn resolve_jump_references(&self, profile: &mut ConnectionProfile) {
        for hop in &mut profile.jumps {
            if let Some(saved) = hop.reference.as_ref().and_then(|reference| {
                self.profiles
                    .iter()
                    .find(|saved| &saved.id == reference && saved.id != profile.id)
            }) {
                hop.host.clone_from(&saved.host);
                hop.user.clone_from(&saved.user);
                hop.port = saved.port;
                hop.identity_files.clone_from(&saved.identity_files);
                hop.ssh.clone_from(&saved.ssh);
            }
        }
    }

    pub fn apply_ssh_defaults(&self, profile: &mut ConnectionProfile) {
        if profile.ssh.term.is_none() {
            profile.ssh.term.clone_from(&self.ssh_terminal_type);
        }
        for ssh in std::iter::once(&mut profile.ssh)
            .chain(profile.jumps.iter_mut().map(|hop| &mut hop.ssh))
        {
            ssh.connect_timeout.get_or_insert(self.ssh_timeout.max(1));
            ssh.keepalive_interval.get_or_insert(self.ssh_keepalive);
            if ssh.identity_agent.is_none() {
                ssh.identity_agent.clone_from(&self.ssh_identity_agent);
            }
        }
    }

    pub fn terminal_type<'a>(&'a self, profile: &'a ConnectionProfile) -> &'a str {
        profile
            .ssh
            .term
            .as_deref()
            .or(self.ssh_terminal_type.as_deref())
            .unwrap_or(DEFAULT_TERMINAL_TYPE)
    }

    pub fn load() -> Result<Self> {
        let path = settings_path()?;
        match File::open(&path) {
            Ok(file) => {
                let mut settings: Self = serde_json::from_reader(file)
                    .with_context(|| format!("Read settings from {}", path.display()))?;
                settings.shortcuts.normalize();
                if settings.linked_configs.is_empty() {
                    for source in settings
                        .profiles
                        .iter()
                        .filter_map(|profile| profile.source.as_ref())
                    {
                        if !settings
                            .linked_configs
                            .iter()
                            .any(|entry| &entry.path == source)
                        {
                            settings.linked_configs.push(LinkedConfig {
                                path: source.clone(),
                                ..LinkedConfig::default()
                            });
                        }
                    }
                }
                Ok(settings)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                theme: ThemePreference::System,
                ..Self::default()
            }),
            Err(error) => Err(error).with_context(|| format!("Open {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let directory = data_dir()?;
        fs::create_dir_all(&directory)?;
        let mut temp = tempfile::NamedTempFile::new_in(&directory)?;
        {
            let mut writer = BufWriter::new(temp.as_file_mut());
            serde_json::to_writer_pretty(&mut writer, self)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        temp.as_file().sync_all()?;
        temp.persist(directory.join("settings.json"))?;
        Ok(())
    }

    pub fn unlink(&mut self, path: &str) {
        self.linked_configs.retain(|source| source.path != path);
        self.profiles
            .retain(|profile| profile.source.as_deref() != Some(path));
    }

    pub fn sync_config(&mut self, result: &ImportResult) -> bool {
        let Some((index, incoming)) = self.config_update(result) else {
            return false;
        };
        self.apply_config_update(index, incoming);
        true
    }

    pub fn synced_configs(&self, results: &[ImportResult]) -> Option<Self> {
        let mut updated = None;
        for result in results {
            let current = updated.as_ref().unwrap_or(self);
            if let Some((index, incoming)) = current.config_update(result) {
                updated
                    .get_or_insert_with(|| self.clone())
                    .apply_config_update(index, incoming);
            }
        }
        updated
    }

    fn config_update(&self, result: &ImportResult) -> Option<(usize, Vec<ConnectionProfile>)> {
        let index = self
            .linked_configs
            .iter()
            .position(|entry| entry.path == result.source)?;
        let path = &self.linked_configs[index].path;
        let existing = self
            .profiles
            .iter()
            .filter(|profile| profile.source.as_ref() == Some(path))
            .collect::<Vec<_>>();
        let excluded = &self.linked_configs[index].excluded;
        let earlier = self
            .profiles
            .iter()
            .filter(|profile| {
                self.linked_configs[..index]
                    .iter()
                    .any(|entry| profile.source.as_ref() == Some(&entry.path))
            })
            .map(|profile| profile.name.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        let mut incoming = result
            .profiles
            .iter()
            .filter(|profile| {
                !excluded.contains(&profile.name)
                    && !earlier.contains(&profile.name.to_ascii_lowercase())
            })
            .collect::<Vec<_>>();
        // Preserve already imported sessions if an update for them was deselected.
        incoming.extend(existing.iter().copied().filter(|profile| {
            excluded.contains(&profile.name)
                && !earlier.contains(&profile.name.to_ascii_lowercase())
                && result
                    .profiles
                    .iter()
                    .any(|candidate| candidate.id == profile.id)
        }));
        incoming.sort_by(|a, b| a.name.cmp(&b.name));
        if existing.iter().copied().eq(incoming.iter().copied()) {
            return None;
        }
        Some((index, incoming.into_iter().cloned().collect()))
    }

    fn apply_config_update(&mut self, index: usize, incoming: Vec<ConnectionProfile>) {
        let path = &self.linked_configs[index].path;
        self.profiles
            .retain(|profile| profile.source.as_ref() != Some(path));
        self.profiles.extend(incoming);
        self.linked_configs[index].synced_at = Some(sync_time());
    }
}

fn data_dir() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Find user settings directory")?
        .join("CapTerm"))
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("settings.json"))
}

pub fn linux_highlights_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("linux-highlights.json"))
}

pub fn sync_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn display_path(path: &str) -> String {
    let Some(extended) = path.strip_prefix(r"\\?\") else {
        return path.to_owned();
    };
    if extended
        .as_bytes()
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"UNC\\"))
    {
        return format!(r"\\{}", &extended[4..]);
    }
    if matches!(extended.as_bytes(), [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic()) {
        return extended.to_owned();
    }
    path.to_owned()
}

pub fn known_hosts() -> Result<PathBuf> {
    let directory = data_dir()?;
    fs::create_dir_all(&directory)?;
    Ok(directory.join("known_hosts"))
}

pub struct ImportResult {
    pub source: String,
    pub profiles: Vec<ConnectionProfile>,
    pub warnings: Vec<String>,
    pub includes: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

pub fn canonical_config_path(path: &str) -> Result<String> {
    let expanded = expand_path(path.trim())?;
    fs::canonicalize(expanded)
        .map(|path| path.to_string_lossy().into_owned())
        .with_context(|| format!("Open SSH config {}", display_path(path)))
}

#[cfg(test)]
pub fn import(path: &str) -> Result<ImportResult> {
    let (mut imports, mut errors) = import_many(&[path.to_owned()]);
    imports.pop().ok_or_else(|| {
        errors
            .pop()
            .map_or_else(|| anyhow!("无法读取 SSH 配置"), |(_, error)| error)
    })
}

pub fn import_many(paths: &[String]) -> (Vec<ImportResult>, Vec<(String, anyhow::Error)>) {
    let Some(home) = dirs::home_dir() else {
        return (Vec::new(), vec![("SSH".into(), anyhow!("找不到用户目录"))]);
    };
    let local_user = env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_default();
    let mut parsed = Vec::new();
    let mut seen = HashSet::new();
    let mut errors = Vec::new();
    let mut owners = BTreeMap::new();
    for path in paths {
        let read = || {
            let source = canonical_config_path(path)?;
            let mut parser = Parser {
                include_dir: home.join(".ssh"),
                remaining_bytes: 2 * 1024 * 1024,
                remaining_files: 256,
                aliases: BTreeSet::new(),
                stack: Vec::new(),
                warnings: BTreeSet::new(),
                includes: BTreeSet::new(),
                skipped: BTreeSet::new(),
            };
            let rules = parser.read(Path::new(&source))?;
            ensure!(
                parser.aliases.len() <= 1024,
                "SSH config has more than 1024 named aliases"
            );
            Ok::<_, anyhow::Error>((source, parser, rules))
        };
        match read() {
            Ok((source, mut parser, rules)) if seen.insert(source.clone()) => {
                for alias in &parser.aliases {
                    let mut options = Options::default();
                    apply(&rules, alias, true, &mut options);
                    if !options.named {
                        parser
                            .skipped
                            .insert((alias.clone(), "仅在未生效的 Include 中定义".into()));
                    } else if owners.contains_key(alias) {
                        parser
                            .skipped
                            .insert((alias.clone(), "已由前面的配置文件定义".into()));
                    } else {
                        owners.insert(alias.clone(), source.clone());
                    }
                }
                parsed.push((source, parser, rules));
            }
            Ok(_) => {}
            Err(error) => errors.push((path.clone(), error)),
        }
    }
    if owners.len() > 4096 {
        errors.push((
            "SSH".into(),
            anyhow!("SSH 配置文件共包含超过 4096 个命名主机"),
        ));
        return (Vec::new(), errors);
    }

    let rules = parsed
        .iter_mut()
        .map(|(_, _, rules)| Rule::Source(std::mem::take(rules)))
        .collect::<Vec<_>>();
    let imports = parsed
        .into_iter()
        .map(|(source, mut parser, _)| {
            let mut profiles = Vec::new();
            for alias in parser
                .aliases
                .iter()
                .filter(|alias| owners.get(*alias) == Some(&source))
            {
                let resolve = || -> Result<ConnectionProfile> {
                    let host = resolve_host(&rules, alias, &local_user, &home, None, None)?;
                    let mut seen = HashSet::from([alias.to_lowercase()]);
                    let jumps =
                        resolve_jumps(&rules, &host, &local_user, &home, &owners, &mut seen, 0)?;
                    Ok(ConnectionProfile {
                        id: format!("ssh-config:{source}:{alias}"),
                        name: alias.clone(),
                        host: host.endpoint.host,
                        user: host.endpoint.user,
                        port: host.endpoint.port,
                        auth: AuthMethod::Key,
                        identity_files: host.endpoint.identity_files,
                        jumps,
                        source: Some(source.clone()),
                        group: String::new(),
                        ssh: host.endpoint.ssh,
                    })
                };
                match resolve() {
                    Ok(profile) => profiles.push(profile),
                    Err(error) => {
                        parser.skipped.insert((alias.clone(), error.to_string()));
                        parser.warnings.insert(format!("{alias}: {error}"));
                    }
                }
            }
            ImportResult {
                source,
                profiles,
                warnings: parser.warnings.into_iter().collect(),
                includes: parser.includes.into_iter().collect(),
                skipped: parser.skipped.into_iter().collect(),
            }
        })
        .collect();
    (imports, errors)
}
fn expand_path(value: &str) -> Result<PathBuf> {
    let value = expand_env(value)?;
    let tail = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"));
    if value == "~" || tail.is_some() {
        let home = dirs::home_dir().context("Find user home directory")?;
        return Ok(tail.map_or_else(|| home.clone(), |tail| home.join(tail)));
    }
    ensure!(
        !value.starts_with('~'),
        "Other users' home paths are unsupported"
    );
    Ok(PathBuf::from(value))
}

fn expand_env(value: &str) -> Result<String> {
    let mut rest = value;
    let mut result = String::new();
    while let Some((prefix, var)) = rest.split_once("${") {
        let (name, tail) = var
            .split_once('}')
            .context("Unclosed environment variable")?;
        result.push_str(prefix);
        result.push_str(&env::var(name).with_context(|| format!("Undefined variable {name}"))?);
        rest = tail;
    }
    result.push_str(rest);
    Ok(result)
}

enum Rule {
    Source(Vec<Rule>),
    Host(Vec<String>),
    Match(bool),
    Include(Vec<Rule>),
    Set(String, String),
    SetEnv(Vec<String>),
    SendEnv(Vec<String>),
    ProxyCommand(Vec<String>),
    Forward(String, Vec<String>),
}

struct Parser {
    include_dir: PathBuf,
    remaining_bytes: usize,
    remaining_files: usize,
    aliases: BTreeSet<String>,
    stack: Vec<PathBuf>,
    warnings: BTreeSet<String>,
    includes: BTreeSet<String>,
    skipped: BTreeSet<(String, String)>,
}

impl Parser {
    fn read(&mut self, path: &Path) -> Result<Vec<Rule>> {
        ensure!(
            self.remaining_files > 0 && self.stack.len() < 16,
            "SSH config Include limit reached"
        );
        let path = fs::canonicalize(path)?;
        let display = display_path(&path.to_string_lossy());
        ensure!(
            !self.stack.contains(&path),
            "SSH config includes itself through {display}"
        );
        self.remaining_files -= 1;
        if !self.stack.is_empty() {
            self.includes.insert(display);
        }
        self.stack.push(path.clone());
        let result = self.read_rules(&path);
        self.stack.pop();
        result
    }

    fn read_rules(&mut self, path: &Path) -> Result<Vec<Rule>> {
        let mut content = String::new();
        File::open(path)?
            .take(self.remaining_bytes as u64 + 1)
            .read_to_string(&mut content)?;
        ensure!(
            content.len() <= self.remaining_bytes,
            "SSH config exceeds the 2 MiB import limit"
        );
        self.remaining_bytes -= content.len();
        let mut rules = Vec::new();
        let display = display_path(&path.to_string_lossy());
        for (index, line) in content.trim_start_matches('\u{feff}').lines().enumerate() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let end = line
                .find(|character: char| character.is_whitespace() || character == '=')
                .unwrap_or(line.len());
            let key = line[..end].to_ascii_lowercase();
            let rest = line[end..]
                .trim_start()
                .strip_prefix('=')
                .unwrap_or(line[end..].trim_start());
            let args = arguments(rest).with_context(|| format!("{display}:{}", index + 1))?;
            let Some(first) = args.first() else {
                continue;
            };
            match key.as_str() {
                "host" => {
                    self.aliases.extend(
                        args.iter()
                            .filter(|name| literal(name))
                            .map(|name| name.to_lowercase()),
                    );
                    self.skipped.extend(
                        args.iter()
                            .filter(|name| name.contains(['*', '?']) && !name.starts_with('!'))
                            .map(|name| (name.clone(), "通配符 Host 已作为默认值合并".into())),
                    );
                    rules.push(Rule::Host(args));
                }
                "match" => {
                    let supported = args.len() == 1 && first.eq_ignore_ascii_case("all");
                    if !supported {
                        self.skipped.insert((
                            format!("Match {}", args.join(" ")),
                            "暂不支持条件匹配".into(),
                        ));
                        self.warnings.insert(format!(
                            "{display}:{}: conditional Match was skipped",
                            index + 1
                        ));
                    }
                    rules.push(Rule::Match(supported));
                }
                "include" => {
                    for entry in args {
                        let value = expand_path(&entry)?;
                        let value = if value.is_absolute() {
                            value
                        } else {
                            self.include_dir.join(value)
                        };
                        let pattern = value.to_str().context("Include path is not Unicode")?;
                        ensure!(
                            !pattern.contains('%'),
                            "Token-expanded Include paths are unsupported"
                        );
                        let mut paths =
                            glob::glob(pattern)?.collect::<std::result::Result<Vec<_>, _>>()?;
                        paths.sort();
                        for included_path in paths {
                            let included = self.read(&included_path).with_context(|| {
                                format!(
                                    "Include {}",
                                    display_path(&included_path.to_string_lossy())
                                )
                            })?;
                            rules.push(Rule::Include(included));
                        }
                    }
                }
                "hostname"
                | "user"
                | "port"
                | "identityfile"
                | "proxyjump"
                | "identityagent"
                | "identitiesonly"
                | "forwardagent"
                | "connecttimeout"
                | "serveraliveinterval"
                | "compression" => {
                    ensure!(
                        args.len() == 1,
                        "{display}:{}: {key} expects one argument",
                        index + 1
                    );
                    rules.push(Rule::Set(key, args.join(" ")));
                }
                "setenv" => rules.push(Rule::SetEnv(args)),
                "sendenv" => rules.push(Rule::SendEnv(args)),
                "proxycommand" => rules.push(Rule::ProxyCommand(args)),
                "localforward" | "remoteforward" | "dynamicforward" => {
                    rules.push(Rule::Forward(key, args))
                }
                "canonicalizehostname"
                | "certificatefile"
                | "hostkeyalias"
                | "userknownhostsfile"
                | "globalknownhostsfile"
                | "remotecommand"
                | "stricthostkeychecking" => {
                    self.warnings
                        .insert(format!("{display}:{}: {key} was not imported", index + 1));
                }
                _ => {}
            }
        }
        Ok(rules)
    }
}

fn arguments(value: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\\' if chars
                .peek()
                .is_some_and(|next| matches!(next, '"' | '\'' | '#' | ' ' | '\t' | '\\')) =>
            {
                current.push(chars.next().expect("escaped character"));
                started = true;
            }
            '"' | '\'' if quote.is_none() => {
                quote = Some(character);
                started = true;
            }
            '"' | '\'' if quote == Some(character) => quote = None,
            '#' if quote.is_none() && !started => break,
            character if character.is_whitespace() && quote.is_none() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            character => {
                current.push(character);
                started = true;
            }
        }
    }
    ensure!(quote.is_none(), "Unclosed quoted argument");
    if started {
        args.push(current);
    }
    Ok(args)
}

fn literal(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('!') && !name.contains(['*', '?'])
}

fn pattern_matches(pattern: &str, name: &str) -> bool {
    let pattern = pattern.as_bytes();
    let name = name.as_bytes();
    let (mut p, mut n) = (0, 0);
    let mut wildcard = None;
    while n < name.len() {
        match pattern.get(p) {
            Some(b'*') => {
                p += 1;
                wildcard = Some((p, n));
            }
            Some(character) if *character == b'?' || character.eq_ignore_ascii_case(&name[n]) => {
                p += 1;
                n += 1;
            }
            _ => {
                let Some((after_star, start)) = wildcard else {
                    return false;
                };
                n = start + 1;
                p = after_star;
                wildcard = Some((p, n));
            }
        }
    }
    pattern[p..].iter().all(|&byte| byte == b'*')
}

#[derive(Default)]
struct Options {
    named: bool,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<String>,
    route: Option<Route>,
    identities: Vec<String>,
    identity_agent: Option<String>,
    identities_only: Option<String>,
    forward_agent: Option<String>,
    connect_timeout: Option<String>,
    keepalive_interval: Option<String>,
    compression: Option<String>,
    set_env: Vec<String>,
    send_env: Vec<String>,
    forwards: Vec<(String, Vec<String>)>,
}

enum Route {
    Jump(String),
    Command(Vec<String>),
}

fn apply(rules: &[Rule], name: &str, initial: bool, options: &mut Options) {
    let mut active = initial;
    for rule in rules {
        match rule {
            Rule::Source(rules) => apply(rules, name, true, options),
            Rule::Host(patterns) => {
                let mut matched = false;
                let mut named = false;
                for pattern in patterns {
                    if let Some(pattern) = pattern.strip_prefix('!') {
                        if pattern_matches(pattern, name) {
                            matched = false;
                            named = false;
                            break;
                        }
                    } else if pattern_matches(pattern, name) {
                        matched = true;
                        named |= literal(pattern);
                    }
                }
                active = matched;
                options.named |= named;
            }
            Rule::Match(matched) => active = *matched,
            Rule::Include(included) if active => apply(included, name, active, options),
            Rule::Set(key, value) if active => {
                let target = match key.as_str() {
                    "hostname" => &mut options.hostname,
                    "user" => &mut options.user,
                    "port" => &mut options.port,
                    "identityagent" => &mut options.identity_agent,
                    "identitiesonly" => &mut options.identities_only,
                    "forwardagent" => &mut options.forward_agent,
                    "connecttimeout" => &mut options.connect_timeout,
                    "serveraliveinterval" => &mut options.keepalive_interval,
                    "compression" => &mut options.compression,
                    "identityfile" => {
                        if !options.identities.contains(value) {
                            options.identities.push(value.clone());
                        }
                        continue;
                    }
                    "proxyjump" => {
                        options
                            .route
                            .get_or_insert_with(|| Route::Jump(value.clone()));
                        continue;
                    }
                    _ => continue,
                };
                target.get_or_insert_with(|| value.clone());
            }
            Rule::SetEnv(values) if active => options.set_env.extend(values.iter().cloned()),
            Rule::SendEnv(values) if active => options.send_env.extend(values.iter().cloned()),
            Rule::ProxyCommand(args) if active => {
                options
                    .route
                    .get_or_insert_with(|| Route::Command(args.clone()));
            }
            Rule::Forward(kind, args) if active => {
                let forward = (kind.clone(), args.clone());
                if !options.forwards.contains(&forward) {
                    options.forwards.push(forward);
                }
            }
            _ => {}
        }
    }
}

struct Resolved {
    alias: String,
    endpoint: Hop,
    route: Option<String>,
}

fn resolve_host(
    rules: &[Rule],
    alias: &str,
    local_user: &str,
    home: &Path,
    user_override: Option<&str>,
    port_override: Option<u16>,
) -> Result<Resolved> {
    let mut options = Options::default();
    apply(rules, alias, true, &mut options);
    let host = tokens(
        options.hostname.as_deref().unwrap_or(alias),
        &[('h', alias)],
    )?;
    ensure!(
        !host.is_empty() && !host.contains(char::is_whitespace),
        "Invalid HostName"
    );
    let user = user_override
        .or(options.user.as_deref())
        .unwrap_or(local_user);
    let port = match (port_override, options.port) {
        (Some(port), _) => port,
        (None, Some(port)) => parse_port(&port)?,
        (None, None) => 22,
    };
    let port_text = port.to_string();
    let values = [
        ('d', home.to_str().context("Home path is not Unicode")?),
        ('h', host.as_str()),
        ('n', alias),
        ('p', &port_text),
        ('r', user),
        ('u', local_user),
    ];
    let identity_files = options
        .identities
        .into_iter()
        .map(|value| {
            if value.eq_ignore_ascii_case("none") {
                return Ok("none".into());
            }
            Ok(expand_path(&tokens(&value, &values)?)?
                .to_string_lossy()
                .into_owned())
        })
        .collect::<Result<_>>()?;
    let mut set_env = BTreeMap::new();
    for value in options.set_env {
        let (name, value) = parse_environment(&value)?;
        set_env
            .entry(name.to_owned())
            .or_insert_with(|| value.to_owned());
    }
    let (route, proxy_command) = match options.route {
        Some(Route::Jump(route)) => (Some(route), None),
        Some(Route::Command(args)) => (None, validate_proxy_command(args)?),
        None => (None, None),
    };
    let port_forwards = options
        .forwards
        .into_iter()
        .map(|(kind, args)| {
            let args = args.iter().map(String::as_str).collect::<Vec<_>>();
            crate::ssh::PortForward::parse(&kind, &args)
        })
        .collect::<Result<_>>()?;
    let ssh = SshOptions {
        identity_agent: options
            .identity_agent
            .map(|value| {
                let value = tokens(&value, &values)?;
                if value.eq_ignore_ascii_case("none")
                    || value == "SSH_AUTH_SOCK"
                    || value.starts_with('$')
                {
                    return Ok(value);
                }
                Ok::<_, anyhow::Error>(expand_path(&value)?.to_string_lossy().into_owned())
            })
            .transpose()?,
        identities_only: options
            .identities_only
            .as_deref()
            .map(parse_ssh_bool)
            .transpose()?
            .unwrap_or(false),
        forward_agent: options
            .forward_agent
            .as_deref()
            .map(parse_ssh_bool)
            .transpose()?
            .unwrap_or(false),
        connect_timeout: options
            .connect_timeout
            .as_deref()
            .map(parse_seconds)
            .transpose()?
            .filter(|value| *value > 0),
        keepalive_interval: options
            .keepalive_interval
            .as_deref()
            .map(parse_seconds)
            .transpose()?,
        compression: options
            .compression
            .as_deref()
            .map(parse_ssh_bool)
            .transpose()?
            .unwrap_or(false),
        set_env,
        send_env: options.send_env,
        term: None,
        proxy_command_alias: proxy_command.as_ref().map(|_| alias.to_owned()),
        proxy_command,
        network_proxy: None,
        port_forwards,
    };
    Ok(Resolved {
        alias: alias.into(),
        endpoint: Hop {
            host,
            user: user.into(),
            port,
            identity_files,
            ssh,
            reference: None,
        },
        route,
    })
}

fn parse_ssh_bool(value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "yes" => Ok(true),
        "no" => Ok(false),
        _ => bail!("SSH 选项必须使用 yes 或 no：{value}"),
    }
}

pub fn parse_seconds(value: &str) -> Result<u32> {
    let mut remaining = value.as_bytes();
    let mut total = 0u32;
    ensure!(!remaining.is_empty(), "请输入有效的时长");
    while !remaining.is_empty() {
        let end = remaining
            .iter()
            .position(|value| !value.is_ascii_digit())
            .unwrap_or(remaining.len());
        let number: u32 = std::str::from_utf8(&remaining[..end])?
            .parse()
            .with_context(|| format!("无效的时长：{value}"))?;
        let unit = match remaining.get(end).map(|byte| byte.to_ascii_lowercase()) {
            None | Some(b's') => 1,
            Some(b'm') => 60,
            Some(b'h') => 3600,
            Some(b'd') => 86400,
            Some(b'w') => 604800,
            _ => bail!("无效的时长单位：{value}"),
        };
        total = number
            .checked_mul(unit)
            .and_then(|seconds| total.checked_add(seconds))
            .context("时长超出范围")?;
        remaining = &remaining[(end + 1).min(remaining.len())..];
    }
    Ok(total)
}

pub fn parse_environment(value: &str) -> Result<(&str, &str)> {
    let (name, value) = value
        .split_once('=')
        .context("环境变量应使用 NAME=value 格式")?;
    let mut bytes = name.bytes();
    ensure!(
        bytes
            .next()
            .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
            && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
            && !value.contains('\0'),
        "无效的环境变量名称：{name}"
    );
    Ok((name, value))
}

fn tokens(value: &str, mapping: &[(char, &str)]) -> Result<String> {
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '%' {
            output.push(character);
            continue;
        }
        let token = chars.next().context("Incomplete SSH config token")?;
        if token == '%' {
            output.push('%');
        } else {
            let (_, value) = mapping
                .iter()
                .find(|(key, _)| *key == token)
                .with_context(|| format!("Unsupported SSH config token %{token}"))?;
            output.push_str(value);
        }
    }
    Ok(output)
}

pub fn parse_proxy_command(text: &str) -> Result<Option<Vec<String>>> {
    validate_proxy_command(arguments(text)?)
}

fn validate_proxy_command(args: Vec<String>) -> Result<Option<Vec<String>>> {
    if args.is_empty() || (args.len() == 1 && args[0].eq_ignore_ascii_case("none")) {
        return Ok(None);
    }
    ensure!(!args[0].is_empty(), "ProxyCommand 需要可执行文件");
    for arg in &args {
        ensure!(
            !arg.chars()
                .any(|ch| ch.is_control() || "&|;<>\x60$*?".contains(ch)),
            "ProxyCommand 使用独立参数，暂不支持 Shell 表达式、管道、重定向或变量展开"
        );
        tokens(
            arg,
            &[('h', "host"), ('p', "22"), ('r', "user"), ('n', "alias")],
        )?;
    }
    Ok(Some(args))
}

pub fn expand_proxy_command(
    args: &[String],
    host: &str,
    port: u16,
    user: &str,
    alias: &str,
) -> Result<Vec<String>> {
    let port = port.to_string();
    args.iter()
        .map(|arg| tokens(arg, &[('h', host), ('p', &port), ('r', user), ('n', alias)]))
        .collect()
}

pub fn format_argv(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.is_empty()
                || arg
                    .chars()
                    .any(|ch| ch.is_whitespace() || ['\'', '"', '\\', '#'].contains(&ch))
            {
                let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
                format!("\"{escaped}\"")
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_port(value: &str) -> Result<u16> {
    let port = value
        .parse::<u16>()
        .with_context(|| format!("Invalid SSH port {value}"))?;
    ensure!(port > 0, "SSH port must be greater than zero");
    Ok(port)
}

pub fn parse_hop(value: &str) -> Result<(&str, Option<&str>, Option<u16>)> {
    let value = value.trim();
    let value = value.strip_prefix("ssh://").unwrap_or(value);
    let (user, address) = value
        .rsplit_once('@')
        .map_or((None, value), |(u, a)| (Some(u), a));
    let (host, port) = if let Some(address) = address.strip_prefix('[') {
        let (host, tail) = address.split_once(']').context("Unclosed IPv6 jump host")?;
        let port = if tail.is_empty() {
            None
        } else {
            Some(parse_port(
                tail.strip_prefix(':').context("Invalid jump host port")?,
            )?)
        };
        (host, port)
    } else if address.matches(':').count() == 1 {
        let (host, port) = address
            .split_once(':')
            .context("Invalid jump host address")?;
        (host, Some(parse_port(port)?))
    } else {
        (address, None)
    };
    ensure!(
        !host.is_empty()
            && !host.contains(['/', '@', ',', '[', ']'])
            && !host.contains(char::is_whitespace),
        "Invalid jump host"
    );
    ensure!(
        !user.is_some_and(|user| user.is_empty()
            || user.contains(['/', '@', ':'])
            || user.contains(char::is_whitespace)),
        "Invalid jump user"
    );
    Ok((host, user, port))
}

fn resolve_jumps(
    rules: &[Rule],
    host: &Resolved,
    local_user: &str,
    home: &Path,
    owners: &BTreeMap<String, String>,
    seen: &mut HashSet<String>,
    depth: usize,
) -> Result<Vec<Hop>> {
    let Some(route) = &host.route else {
        return Ok(Vec::new());
    };
    if route.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    ensure!(depth < 8, "ProxyJump route is too deep");
    let port = host.endpoint.port.to_string();
    let route = tokens(
        route,
        &[
            ('h', &host.endpoint.host),
            ('n', &host.alias),
            ('p', &port),
            ('r', &host.endpoint.user),
        ],
    )?;
    let mut jumps = Vec::new();
    for (index, jump) in route.split(',').enumerate() {
        let (alias, user, port) = parse_hop(jump)?;
        if !seen.insert(alias.to_lowercase()) {
            bail!("ProxyJump loop through {alias}");
        }
        let mut resolved = resolve_host(rules, alias, local_user, home, user, port)
            .map_err(|error| anyhow!("Jump host {alias}: {error}"))?;
        if index == 0 {
            jumps.extend(resolve_jumps(
                rules,
                &resolved,
                local_user,
                home,
                owners,
                seen,
                depth + 1,
            )?);
        }
        if user.is_none() && port.is_none() {
            let alias = alias.to_lowercase();
            if let Some(source) = owners.get(&alias) {
                resolved.endpoint.reference = Some(format!("ssh-config:{source}:{alias}"));
            }
        }
        jumps.push(resolved.endpoint);
        ensure!(jumps.len() <= 8, "ProxyJump supports up to eight hops");
    }
    Ok(jumps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_settings_default_to_ten_thousand_scrollback_rows() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.scrollback_lines(), 10_000);
    }

    #[test]
    fn legacy_settings_use_clipboard_confirmations() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.osc52_write, Osc52WritePreference::Ask);
        assert!(settings.confirm_multiline_paste);
    }

    #[test]
    fn always_allow_clipboard_writes_survives_saving_settings() -> Result<()> {
        let settings = Settings {
            osc52_write: Osc52WritePreference::Allow,
            ..Settings::default()
        };
        let saved = serde_json::to_value(settings)?;
        assert_eq!(saved["osc52_write"], "allow");
        let loaded: Settings = serde_json::from_value(saved)?;
        assert_eq!(loaded.osc52_write, Osc52WritePreference::Allow);
        Ok(())
    }

    #[test]
    fn ssh_defaults_apply_without_overwriting_session_and_jump_overrides() -> Result<()> {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "ssh_terminal_type": "vt100",
            "ssh_identity_agent": "none",
            "ssh_keepalive": 0
        }))?;
        let mut profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "fixture",
            "name": "target",
            "host": "target",
            "user": "user",
            "port": 22,
            "jumps": [{
                "host": "jump",
                "user": "user",
                "port": 22,
                "ssh": {"identity_agent": "SSH_AUTH_SOCK"}
            }]
        }))?;
        assert_eq!(settings.terminal_type(&profile), "vt100");
        settings.apply_ssh_defaults(&mut profile);
        assert_eq!(profile.ssh.term.as_deref(), Some("vt100"));
        assert_eq!(profile.ssh.identity_agent.as_deref(), Some("none"));
        assert_eq!(profile.ssh.keepalive_interval, Some(0));
        assert_eq!(
            profile.jumps[0].ssh.identity_agent.as_deref(),
            Some("SSH_AUTH_SOCK")
        );
        profile.ssh.term = Some("xterm".into());
        settings.apply_ssh_defaults(&mut profile);
        assert_eq!(settings.terminal_type(&profile), "xterm");
        Ok(())
    }

    #[test]
    fn displays_windows_drive_and_unc_paths_without_the_extended_prefix() {
        for (stored, displayed) in [
            (
                r"\\?\C:\Users\jack\.ssh\config",
                r"C:\Users\jack\.ssh\config",
            ),
            (r"\\?\UNC\server\share\config", r"\\server\share\config"),
            (r"\\?\unc\server\share\config", r"\\server\share\config"),
            (r"\\server\share\config", r"\\server\share\config"),
            (r"\\?\Volume{guid}\config", r"\\?\Volume{guid}\config"),
            ("/home/user/.ssh/config", "/home/user/.ssh/config"),
        ] {
            assert_eq!(display_path(stored), displayed);
        }
    }

    #[cfg(windows)]
    #[test]
    fn displaying_an_import_path_does_not_change_its_source_identity() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config");
        fs::write(&path, "Host app\n StrictHostKeyChecking yes\n")?;
        let canonical = canonical_config_path(&path.to_string_lossy())?;
        let displayed = display_path(&canonical);
        let (files, errors) = import_many(&[displayed.clone(), canonical.clone()]);
        assert!(errors.is_empty(), "{errors:?}");
        let [file] = files.as_slice() else {
            panic!("one source for both path forms")
        };
        assert_eq!(file.source, canonical);
        assert_eq!(file.profiles[0].id, format!("ssh-config:{canonical}:app"));
        assert!(file.warnings[0].starts_with(&format!("{displayed}:2:")));
        Ok(())
    }

    #[test]
    fn resolves_aliases_wildcards_and_ordered_jump_route() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let included = directory.path().join("edge.conf");
        let config = directory.path().join("ssh_config");
        fs::write(
            &included,
            "Host bastion\n HostName edge.example.com\n User ops\n IdentityFile ~/edge.key\n",
        )?;
        fs::write(
            &config,
            format!(
                "Include \"{}\"\nHost prod prod-* !prod-excluded\n HostName node.example.com\n Port 2222\n ProxyJump bastion,build@relay:2200\n IdentityFile \"~/key with space\"\nHost *\n User deploy\n Port 22\n IdentityFile ~/fallback\n",
                included.display()
            ),
        )?;
        let imported = import(config.to_str().unwrap())?;
        assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
        let prod = imported
            .profiles
            .iter()
            .find(|profile| profile.name == "prod")
            .unwrap();
        assert_eq!(
            (&*prod.host, &*prod.user, prod.port),
            ("node.example.com", "deploy", 2222)
        );
        assert!(prod.identity_files[0].ends_with("key with space"));
        assert_eq!(prod.identity_files.len(), 2);
        assert_eq!(prod.jumps.len(), 2);
        assert_eq!(
            (&*prod.jumps[0].host, &*prod.jumps[0].user),
            ("edge.example.com", "ops")
        );
        assert_eq!(
            (
                &*prod.jumps[1].host,
                &*prod.jumps[1].user,
                prod.jumps[1].port
            ),
            ("relay", "build", 2200)
        );
        Ok(())
    }

    #[test]
    fn reports_unsupported_routes_and_cycles_without_executing_commands() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let config = directory.path().join("ssh_config");
        fs::write(
            &config,
            "Host command\n ProxyCommand echo bad | cat\nHost loop\n ProxyJump loop\nMatch exec \"exit 9\"\n User ignored\nHost okay\n User allowed\n",
        )?;
        let imported = import(config.to_str().unwrap())?;
        assert_eq!(imported.profiles.len(), 1);
        assert_eq!(imported.profiles[0].user, "allowed");
        assert!(
            imported
                .warnings
                .iter()
                .any(|warning| warning.contains("ProxyCommand"))
        );
        assert!(
            imported
                .warnings
                .iter()
                .any(|warning| warning.contains("ProxyJump loop"))
        );
        assert!(
            imported
                .warnings
                .iter()
                .any(|warning| warning.contains("Match"))
        );
        assert_eq!(
            parse_hop("ssh://ops@[::1]:2222")?,
            ("::1", Some("ops"), Some(2222))
        );
        Ok(())
    }

    #[test]
    fn resolves_later_defaults_and_jump_references_across_ordered_files() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::write(&first, "Host app\n HostName app.example\n ProxyJump edge\n")?;
        fs::write(
            &second,
            "Host app\n HostName ignored.example\n User deploy\nHost edge\n HostName edge.example\nHost *\n Port 2222\n",
        )?;
        let (sources, errors) = import_many(&[
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        ]);
        assert!(errors.is_empty(), "{errors:?}");
        let [first, second] = sources.as_slice() else {
            panic!("two ordered sources")
        };
        let app = &first.profiles[0];
        let edge = &second.profiles[0];
        assert_eq!(
            (&*app.host, &*app.user, app.port),
            ("app.example", "deploy", 2222)
        );
        assert_eq!(edge.name, "edge");
        assert_eq!(edge.port, 2222);
        assert_eq!(app.jumps[0].host, "edge.example");
        assert_eq!(app.jumps[0].reference.as_ref(), Some(&edge.id));
        assert!(second.skipped.iter().any(|(name, _)| name == "app"));
        Ok(())
    }

    #[test]
    fn maps_authentication_timing_environment_and_proxy_without_running_programs() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("ssh_config");
        fs::write(
            &file,
            r#"Host via
    ProxyJump edge
    SetEnv LANG=zh_CN.UTF-8 "TEAM_NAME=Blue Team"
    SendEnv LANG LC_*
    LocalForward 127.0.0.1:8080 localhost:80
Host edge
    HostName edge.example.com
    User ops
    Port 2222
    ProxyCommand tunnel-client --host %h --label "edge route" --port %p
    IdentityAgent none
    IdentitiesOnly yes
    ConnectTimeout 1m5s
    ServerAliveInterval 0
    Compression yes
"#,
        )?;
        let imported = import(file.to_str().unwrap())?;
        assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
        let via = imported
            .profiles
            .iter()
            .find(|profile| profile.name == "via")
            .unwrap();
        let edge = imported
            .profiles
            .iter()
            .find(|profile| profile.name == "edge")
            .unwrap();
        assert_eq!(via.jumps[0].reference.as_ref(), Some(&edge.id));
        let command = via.jumps[0].ssh.proxy_command.as_ref().unwrap();
        assert_eq!(
            expand_proxy_command(command, "edge.example.com", 2222, "ops", "edge")?,
            [
                "tunnel-client",
                "--host",
                "edge.example.com",
                "--label",
                "edge route",
                "--port",
                "2222"
            ]
        );
        assert_eq!(
            parse_proxy_command(&format_argv(command))?.as_ref(),
            Some(command)
        );
        assert!(edge.ssh.identities_only && edge.ssh.compression);
        assert_eq!(edge.ssh.identity_agent.as_deref(), Some("none"));
        assert_eq!(edge.ssh.connect_timeout, Some(65));
        assert_eq!(edge.ssh.keepalive_interval, Some(0));
        assert_eq!(via.ssh.set_env["TEAM_NAME"], "Blue Team");
        assert_eq!(via.ssh.send_env, ["LANG", "LC_*"]);
        assert_eq!(via.ssh.port_forwards.len(), 1);
        Ok(())
    }

    #[test]
    fn linked_config_keeps_deselected_updates_and_first_source_precedence() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::write(&first, "Host shared\n HostName original.example\n")?;
        fs::write(
            &second,
            "Host shared\n HostName second.example\nHost unique\n HostName unique.example\n",
        )?;
        let first_result = import(first.to_str().unwrap())?;
        let second_result = import(second.to_str().unwrap())?;
        let settings = Settings {
            profiles: first_result.profiles,
            linked_configs: vec![
                LinkedConfig {
                    path: first_result.source,
                    excluded: BTreeSet::from(["shared".into()]),
                    synced_at: None,
                },
                LinkedConfig {
                    path: second_result.source.clone(),
                    ..LinkedConfig::default()
                },
            ],
            ..Settings::default()
        };
        fs::write(
            &first,
            "Host shared\n HostName changed.example\nHost added\n HostName added.example\n",
        )?;
        let first_result = import(first.to_str().unwrap())?;
        let results = [first_result, second_result];
        let mut settings = settings.synced_configs(&results).unwrap();
        assert!(settings.synced_configs(&results).is_none());
        assert_eq!(
            settings
                .profiles
                .iter()
                .map(|p| (&*p.name, &*p.host))
                .collect::<Vec<_>>(),
            [
                ("added", "added.example"),
                ("shared", "original.example"),
                ("unique", "unique.example"),
            ]
        );
        settings.unlink(&results[0].source);
        assert!(settings.sync_config(&results[1]));
        assert_eq!(settings.profiles[0].host, "second.example");
        Ok(())
    }
}
