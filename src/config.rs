use std::{
    collections::{BTreeSet, HashSet},
    env,
    fs::{self, File},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hop {
    pub host: String,
    pub user: String,
    pub port: u16,
    #[serde(default)]
    pub identity_files: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    #[serde(default)]
    pub identity_files: Vec<String>,
    #[serde(default)]
    pub jumps: Vec<Hop>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub profiles: Vec<ConnectionProfile>,
    pub config_path: String,
    pub light: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            config_path: "~/.ssh/config".into(),
            light: false,
        }
    }
}

impl Settings {
    pub fn load() -> Result<Self> {
        let path = data_dir()?.join("settings.json");
        match File::open(&path) {
            Ok(file) => serde_json::from_reader(file)
                .with_context(|| format!("Read settings from {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
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
}

fn data_dir() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Find user settings directory")?
        .join("CapTerm"))
}

pub fn known_hosts() -> Result<PathBuf> {
    let directory = data_dir()?;
    fs::create_dir_all(&directory)?;
    Ok(directory.join("known_hosts"))
}

pub struct ImportResult {
    pub profiles: Vec<ConnectionProfile>,
    pub warnings: Vec<String>,
}

pub fn import(path: &str) -> Result<ImportResult> {
    let path = fs::canonicalize(expand_path(path.trim())?)
        .with_context(|| format!("Open SSH config {path}"))?;
    let home = dirs::home_dir().context("Find user home directory")?;
    let local_user = env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_default();
    let mut parser = Parser {
        include_dir: home.join(".ssh"),
        remaining_bytes: 2 * 1024 * 1024,
        remaining_files: 256,
        aliases: BTreeSet::new(),
        stack: Vec::new(),
        warnings: BTreeSet::new(),
    };
    let rules = parser.read(&path)?;
    ensure!(
        parser.aliases.len() <= 1024,
        "SSH config has more than 1024 named aliases"
    );
    let source = path.to_string_lossy().into_owned();
    let mut profiles = Vec::new();
    for alias in &parser.aliases {
        let resolve = || -> Result<ConnectionProfile> {
            let host = resolve_host(&rules, alias, &local_user, &home, None, None)?;
            ensure!(host.named, "alias appears only in an inactive Include");
            let mut seen = HashSet::from([alias.to_lowercase()]);
            let jumps = resolve_jumps(&rules, &host, &local_user, &home, &mut seen, 0)?;
            Ok(ConnectionProfile {
                id: format!("ssh-config:{source}:{alias}"),
                name: alias.clone(),
                host: host.endpoint.host,
                user: host.endpoint.user,
                port: host.endpoint.port,
                identity_files: host.endpoint.identity_files,
                jumps,
                source: Some(source.clone()),
            })
        };
        match resolve() {
            Ok(profile) => profiles.push(profile),
            Err(error) => {
                parser.warnings.insert(format!("{alias}: {error}"));
            }
        }
    }
    Ok(ImportResult {
        profiles,
        warnings: parser.warnings.into_iter().collect(),
    })
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
    Host(Vec<String>),
    Match(bool),
    Include(Vec<Rule>),
    Set(String, String),
}

struct Parser {
    include_dir: PathBuf,
    remaining_bytes: usize,
    remaining_files: usize,
    aliases: BTreeSet<String>,
    stack: Vec<PathBuf>,
    warnings: BTreeSet<String>,
}

impl Parser {
    fn read(&mut self, path: &Path) -> Result<Vec<Rule>> {
        ensure!(
            self.remaining_files > 0 && self.stack.len() < 16,
            "SSH config Include limit reached"
        );
        let path = fs::canonicalize(path)?;
        ensure!(
            !self.stack.contains(&path),
            "SSH config includes itself through {}",
            path.display()
        );
        self.remaining_files -= 1;
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
            let args =
                arguments(rest).with_context(|| format!("{}:{}", path.display(), index + 1))?;
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
                    rules.push(Rule::Host(args));
                }
                "match" => {
                    let supported = args.len() == 1 && first.eq_ignore_ascii_case("all");
                    if !supported {
                        self.warnings.insert(format!(
                            "{}:{}: conditional Match was skipped",
                            path.display(),
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
                            let included = self
                                .read(&included_path)
                                .with_context(|| format!("Include {}", included_path.display()))?;
                            rules.push(Rule::Include(included));
                        }
                    }
                }
                "hostname" | "user" | "port" | "identityfile" | "proxyjump" | "proxycommand" => {
                    ensure!(
                        args.len() == 1 || key == "proxycommand",
                        "{}:{}: {key} expects one argument",
                        path.display(),
                        index + 1
                    );
                    rules.push(Rule::Set(key, args.join(" ")));
                }
                "canonicalizehostname"
                | "certificatefile"
                | "hostkeyalias"
                | "identityagent"
                | "identitiesonly"
                | "userknownhostsfile"
                | "globalknownhostsfile"
                | "remotecommand"
                | "stricthostkeychecking"
                | "localforward"
                | "remoteforward"
                | "dynamicforward" => {
                    self.warnings.insert(format!(
                        "{}:{}: {key} was not imported",
                        path.display(),
                        index + 1
                    ));
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
    route: Option<(String, String)>,
    identities: Vec<String>,
}

fn apply(rules: &[Rule], name: &str, initial: bool, options: &mut Options) {
    let mut active = initial;
    for rule in rules {
        match rule {
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
                    "identityfile" => {
                        if !options.identities.contains(value) {
                            options.identities.push(value.clone());
                        }
                        continue;
                    }
                    "proxycommand" | "proxyjump" => {
                        options
                            .route
                            .get_or_insert_with(|| (key.clone(), value.clone()));
                        continue;
                    }
                    _ => continue,
                };
                target.get_or_insert_with(|| value.clone());
            }
            _ => {}
        }
    }
}

struct Resolved {
    alias: String,
    endpoint: Hop,
    named: bool,
    route: Option<(String, String)>,
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
    Ok(Resolved {
        alias: alias.into(),
        endpoint: Hop {
            host,
            user: user.into(),
            port,
            identity_files,
        },
        named: options.named,
        route: options.route,
    })
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
    seen: &mut HashSet<String>,
    depth: usize,
) -> Result<Vec<Hop>> {
    let Some((kind, route)) = &host.route else {
        return Ok(Vec::new());
    };
    if route.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    ensure!(
        kind == "proxyjump",
        "ProxyCommand is unsupported; configure a ProxyJump route"
    );
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
        let resolved = resolve_host(rules, alias, local_user, home, user, port)
            .map_err(|error| anyhow!("Jump host {alias}: {error}"))?;
        if index == 0 {
            jumps.extend(resolve_jumps(
                rules,
                &resolved,
                local_user,
                home,
                seen,
                depth + 1,
            )?);
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
            "Host command\n ProxyCommand echo bad\nHost loop\n ProxyJump loop\nMatch exec \"exit 9\"\n User ignored\nHost okay\n User allowed\n",
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
}
