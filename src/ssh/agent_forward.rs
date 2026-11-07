use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use russh::{
    Channel, ChannelOpenFailure, client,
    keys::agent::{
        AgentSessionBinding,
        client::{AgentClient, AgentStream},
    },
};
use tokio::{
    io,
    sync::{Semaphore, watch},
    time::timeout,
};

const AGENT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FORWARDED_CHANNELS: usize = 16;

pub(super) struct AgentForwarding {
    socket: PathBuf,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    closed: bool,
    active: Weak<AgentLease>,
}

/// Only a live shell which has successfully requested forwarding holds this lease.
pub(super) struct AgentLease {
    binding: AgentSessionBinding,
    stop: watch::Sender<bool>,
    channels: Arc<Semaphore>,
}

impl AgentForwarding {
    pub(super) fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            state: Mutex::new(State::default()),
        }
    }

    pub(super) async fn verify(&self, binding: &AgentSessionBinding) -> Result<()> {
        let _ = bound_agent(&self.socket, binding).await?;
        Ok(())
    }

    pub(super) fn activate(&self, binding: &AgentSessionBinding) -> Result<Arc<AgentLease>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("SSH Agent 转发状态不可用"))?;
        ensure!(!state.closed, "SSH 连接已关闭");
        if let Some(active) = state.active.upgrade() {
            return Ok(active);
        }
        let active = Arc::new(AgentLease {
            binding: binding.clone(),
            stop: watch::channel(false).0,
            channels: Arc::new(Semaphore::new(MAX_FORWARDED_CHANNELS)),
        });
        state.active = Arc::downgrade(&active);
        Ok(active)
    }

    pub(super) fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            if let Some(active) = state.active.upgrade() {
                active.stop.send_replace(true);
            }
            state.active = Weak::new();
        }
    }

    pub(super) async fn handle(
        &self,
        channel: Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
    ) {
        let active = self
            .state
            .lock()
            .ok()
            .and_then(|state| state.active.upgrade());
        let Some(active) = active else {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return;
        };
        let Ok(permit) = active.channels.clone().try_acquire_owned() else {
            reply.reject(ChannelOpenFailure::ResourceShortage).await;
            return;
        };
        let mut stop = active.stop.subscribe();
        let binding = active.binding.clone();
        // The relay watches the lease but must not keep it alive after the shell exits.
        drop(active);
        let socket = self.socket.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if *stop.borrow() {
                reply
                    .reject(ChannelOpenFailure::AdministrativelyProhibited)
                    .await;
                return;
            }
            let agent = tokio::select! {
                biased;
                _ = stop.changed() => {
                    reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
                    return;
                },
                result = bound_agent(&socket, &binding) => result,
            };
            let Ok(mut agent) = agent else {
                reply.reject(ChannelOpenFailure::ConnectFailed).await;
                return;
            };
            if stop.has_changed().unwrap_or(true) {
                reply
                    .reject(ChannelOpenFailure::AdministrativelyProhibited)
                    .await;
                return;
            }
            reply.accept().await;
            let mut remote = channel.into_stream();
            tokio::select! {
                biased;
                _ = stop.changed() => {}
                _ = io::copy_bidirectional(&mut remote, &mut agent) => {}
            }
        });
    }
}

type LocalAgent = Box<dyn AgentStream + Send + Unpin>;

async fn bound_agent(socket: &Path, binding: &AgentSessionBinding) -> Result<LocalAgent> {
    timeout(AGENT_TIMEOUT, async {
        #[cfg(windows)]
        let mut agent = AgentClient::connect_named_pipe(socket.as_os_str())
            .await
            .context("无法连接本机 OpenSSH Agent")?;
        #[cfg(unix)]
        let mut agent = AgentClient::connect_uds(socket)
            .await
            .context("无法连接本机 OpenSSH Agent")?;
        agent
            .bind_session(binding, true)
            .await
            .context("本机 OpenSSH Agent 拒绝绑定此 SSH 会话，无法安全转发")?;
        Ok(agent.into_inner())
    })
    .await
    .context("连接并绑定本机 OpenSSH Agent 超时")?
}
