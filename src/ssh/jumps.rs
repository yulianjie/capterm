use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, Weak},
};

use russh::{client, keys::PublicKey};

use super::{HOST_KEY_STORE, HostVerifier, read_host_pins};
use crate::config::Hop;

#[derive(Eq, Hash, PartialEq)]
pub(super) struct JumpKey {
    known_hosts: PathBuf,
    prefix: String,
}

impl JumpKey {
    pub(super) fn new(known_hosts: &Path, prefix: &[Hop]) -> Option<Self> {
        let endpoints: Vec<_> = prefix
            .iter()
            .map(|hop| {
                let ssh = &hop.ssh;
                (
                    &hop.host,
                    &hop.user,
                    hop.port,
                    &hop.identity_files,
                    ssh.identities_only,
                    &ssh.identity_agent,
                    ssh.connect_timeout,
                    ssh.keepalive_interval,
                    ssh.compression,
                )
            })
            .collect();
        Some(Self {
            known_hosts: known_hosts.to_owned(),
            prefix: serde_json::to_string(&endpoints).ok()?,
        })
    }
}

pub(super) struct SharedHop {
    pub(super) session: client::Handle<HostVerifier>,
    pub(super) authentication: &'static str,
    pub(super) cipher: Option<String>,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) persistent_key: Option<PublicKey>,
    // A nested connection is reusable only with the same upstream transport.
    pub(super) parent: Option<Arc<SharedHop>>,
}

impl SharedHop {
    fn has_persistent_route(&self) -> bool {
        self.persistent_key.is_some()
            && self
                .parent
                .as_ref()
                .is_none_or(|hop| hop.has_persistent_route())
    }
}

static JUMPS: LazyLock<Mutex<HashMap<JumpKey, Weak<SharedHop>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) async fn reuse(
    key: &JumpKey,
    parent: Option<&Arc<SharedHop>>,
) -> Option<Arc<SharedHop>> {
    let hop = JUMPS.lock().ok()?.get(key)?.upgrade()?;
    let same_parent = match (hop.parent.as_ref(), parent) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    };
    if hop.session.is_closed() || !same_parent {
        return None;
    }
    let persistent_key = hop.persistent_key.as_ref()?;
    let _guard = HOST_KEY_STORE.lock().await;
    let pins = read_host_pins(&hop.host, hop.port, &key.known_hosts).ok()?;
    let pinned = pins
        .iter()
        .any(|(_, saved)| saved.key_data() == persistent_key.key_data());
    (pinned && !hop.session.is_closed()).then_some(hop)
}

pub(super) fn remember(key: JumpKey, hop: &Arc<SharedHop>) {
    if !hop.has_persistent_route() {
        return;
    }
    if let Ok(mut cache) = JUMPS.lock() {
        cache.retain(|_, hop| hop.strong_count() > 0);
        cache.insert(key, Arc::downgrade(hop));
    }
}
