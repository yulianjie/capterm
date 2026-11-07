use std::sync::mpsc::Sender;

use anyhow::Result;
use tokio::sync::mpsc::Receiver;

use crate::ssh::{ShellCommand, ShellEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalShell {
    PowerShell,
    CommandPrompt,
    Wsl,
}

impl LocalShell {
    pub fn label(self) -> &'static str {
        match self {
            Self::PowerShell => "PowerShell",
            Self::CommandPrompt => "Command Prompt",
            Self::Wsl => "WSL",
        }
    }
}

#[cfg(windows)]
pub use windows_pty::open;
#[cfg(windows)]
mod windows_console;

#[cfg(not(windows))]
pub async fn open(
    shell: LocalShell,
    _columns: u16,
    _rows: u16,
) -> Result<(Sender<ShellCommand>, Receiver<ShellEvent>)> {
    anyhow::bail!("Local {} sessions require Windows", shell.label());
}

#[cfg(windows)]
mod windows_pty {
    use std::{
        ffi::OsString,
        io::{self, Read, Write},
        os::windows::ffi::OsStringExt,
        path::PathBuf,
        sync::mpsc::{self, RecvTimeoutError},
        thread,
        time::Duration,
    };

    use anyhow::{Context, ensure};
    use portable_pty::{Child, CommandBuilder, PtyPair, PtySize, native_pty_system};
    use tokio::sync::{mpsc as async_mpsc, oneshot};
    use windows::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{SystemInformation::GetSystemDirectoryW, Threading::WaitForSingleObject},
    };

    use super::*;

    pub async fn open(
        shell: LocalShell,
        columns: u16,
        rows: u16,
    ) -> Result<(Sender<ShellCommand>, Receiver<ShellEvent>)> {
        let (commands, incoming) = mpsc::channel();
        let (events, output) = async_mpsc::channel(32);
        let (ready, launched) = oneshot::channel();
        thread::Builder::new()
            .name("capterm-local-shell".into())
            .spawn(move || {
                let mut ready = Some(ready);
                let result = serve(shell, size(columns, rows), incoming, &events, &mut ready);
                if let Some(ready) = ready {
                    let _ = ready.send(result.map(|_| ()));
                } else {
                    let event = match result {
                        Ok(code) => ShellEvent::Exited(code),
                        Err(error) => ShellEvent::Error(format!("{error:#}")),
                    };
                    let _ = events.blocking_send(event);
                }
            })
            .context("Starting local shell worker")?;
        launched.await.context("Local shell worker stopped")??;
        Ok((commands, output))
    }

    fn serve(
        shell: LocalShell,
        initial_size: PtySize,
        commands: mpsc::Receiver<ShellCommand>,
        events: &async_mpsc::Sender<ShellEvent>,
        ready: &mut Option<oneshot::Sender<Result<()>>>,
    ) -> Result<Option<u32>> {
        let command = command(shell)?;
        windows_console::prepare()?;
        let PtyPair { master, slave } = native_pty_system()
            .openpty(initial_size)
            .context("Opening Windows pseudoconsole")?;
        let mut reader = master
            .try_clone_reader()
            .context("Opening terminal output")?;
        let mut writer = master.take_writer().context("Opening terminal input")?;

        // The output pipe must keep draining while the pseudoconsole closes.
        let output = events.clone();
        let reading = thread::Builder::new()
            .name("capterm-local-output".into())
            .spawn(move || drain_output(&mut reader, &output))
            .context("Starting local terminal reader")?;
        let (input, pending_input) = mpsc::channel::<Vec<u8>>();
        let writing = thread::Builder::new()
            .name("capterm-local-input".into())
            .spawn(move || -> io::Result<()> {
                for bytes in pending_input {
                    writer.write_all(&bytes)?;
                }
                Ok(())
            })
            .context("Starting local terminal writer")?;

        let mut child = slave
            .spawn_command(command)
            .with_context(|| format!("Launching {}", shell.label()))?;
        drop(slave);
        let ready = ready.take().expect("shell startup is reported once");
        let result = if ready.send(Ok(())).is_ok() {
            (|| loop {
                if let Some(code) = exit_code(child.as_mut(), 0)? {
                    break Ok(Some(code));
                }
                if events.is_closed() {
                    break Ok(None);
                }
                if reading.is_finished() {
                    break closed_pipe(child.as_mut(), "output");
                }
                if writing.is_finished() {
                    break closed_pipe(child.as_mut(), "input");
                }
                match commands.recv_timeout(Duration::from_millis(50)) {
                    Ok(ShellCommand::Input(bytes)) => {
                        if input.send(bytes).is_err() {
                            break closed_pipe(child.as_mut(), "input");
                        }
                    }
                    Ok(ShellCommand::Resize { columns, rows }) => {
                        master
                            .resize(size(columns, rows))
                            .context("Resizing local terminal")?;
                    }
                    Ok(ShellCommand::Close) | Err(RecvTimeoutError::Disconnected) => {
                        break Ok(None);
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
            })()
        } else {
            Ok(None)
        };

        if !matches!(result, Ok(Some(_))) {
            let _ = child.kill();
        }
        drop(master);
        drop(input);
        let _ = child.wait();
        let output = reading
            .join()
            .map_err(|_| anyhow::anyhow!("Local reader stopped"));
        let _ = writing.join();
        let code = result?;
        output?.context("Reading local terminal output")?;
        Ok(code)
    }

    fn command(shell: LocalShell) -> Result<CommandBuilder> {
        let mut system = [0; 260];
        // The buffer stays valid for the call and contains UTF-16 on success.
        let length = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
        if length == 0 {
            return Err(io::Error::last_os_error()).context("Finding the Windows system directory");
        }
        ensure!(
            length < system.len(),
            "Windows system directory is too long"
        );
        let system = PathBuf::from(OsString::from_wide(&system[..length]));
        let mut command = CommandBuilder::new(system.join(match shell {
            LocalShell::PowerShell => r"WindowsPowerShell\v1.0\powershell.exe",
            LocalShell::CommandPrompt => "cmd.exe",
            LocalShell::Wsl => "wsl.exe",
        }));
        match shell {
            LocalShell::PowerShell => command.arg("-NoLogo"),
            LocalShell::CommandPrompt => {}
            LocalShell::Wsl => command.args(["--cd", "~"]),
        }
        Ok(command)
    }

    fn size(columns: u16, rows: u16) -> PtySize {
        PtySize {
            cols: columns.clamp(1, i16::MAX as u16),
            rows: rows.clamp(1, i16::MAX as u16),
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    fn exit_code(child: &mut dyn Child, wait_ms: u32) -> Result<Option<u32>> {
        let process = child
            .as_raw_handle()
            .context("Local shell has no process handle")?;
        // The child keeps its handle alive throughout the wait.
        match unsafe { WaitForSingleObject(HANDLE(process.cast()), wait_ms) } {
            WAIT_OBJECT_0 => Ok(Some(
                child.wait().context("Waiting for local shell")?.exit_code(),
            )),
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()).context("Checking local shell process"),
        }
    }

    fn closed_pipe(child: &mut dyn Child, pipe: &str) -> Result<Option<u32>> {
        // ConPTY can close its pipes before Windows signals an ordinary shell exit.
        let code =
            exit_code(child, 500)?.with_context(|| format!("Local terminal {pipe} closed"))?;
        Ok(Some(code))
    }

    fn drain_output(
        reader: &mut impl Read,
        events: &async_mpsc::Sender<ShellEvent>,
    ) -> io::Result<()> {
        let mut buffer = [0; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(count) => {
                    if !events.is_closed() {
                        let _ = events.blocking_send(ShellEvent::Output(buffer[..count].to_vec()));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::time::Duration;

    use anyhow::{Context, bail, ensure};
    use tokio::time::timeout;

    use super::*;

    async fn check_shell(
        shell: LocalShell,
        prompt: &[u8],
        input: &[u8],
        expected: &[u8],
        expected_exit: u32,
    ) -> Result<()> {
        let mut received = Vec::new();
        timeout(Duration::from_secs(30), async {
            let (commands, mut output) = open(shell, 80, 25).await?;
            let cursor_query = b"\x1b[6n";
            let mut sent_input = false;
            while let Some(event) = output.recv().await {
                match event {
                    ShellEvent::Output(bytes) => {
                        let previous = received.len();
                        received.extend_from_slice(&bytes);
                        let tail = &received[previous.saturating_sub(cursor_query.len() - 1)..];
                        if tail
                            .windows(cursor_query.len())
                            .any(|bytes| bytes == cursor_query)
                        {
                            commands.send(ShellCommand::Input(b"\x1b[1;1R".to_vec()))?;
                        }
                        if !sent_input
                            && received.windows(prompt.len()).any(|bytes| bytes == prompt)
                        {
                            sent_input = true;
                            commands.send(ShellCommand::Resize {
                                columns: 103,
                                rows: 33,
                            })?;
                            commands.send(ShellCommand::Input(input.to_vec()))?;
                        }
                    }
                    ShellEvent::Exited(Some(code)) => {
                        ensure!(code == expected_exit, "Unexpected exit status {code}");
                        ensure!(
                            received
                                .windows(expected.len())
                                .any(|bytes| bytes == expected),
                            "Missing command output"
                        );
                        return Ok(());
                    }
                    ShellEvent::Exited(None) => bail!("Local shell closed without an exit status"),
                    ShellEvent::Error(error) => bail!("{error}"),
                }
            }
            bail!("Local shell output closed without an exit event")
        })
        .await
        .context("Local shell timed out")
        .flatten()
        .with_context(|| {
            format!(
                "{} output {:?}",
                shell.label(),
                String::from_utf8_lossy(&received)
            )
        })
    }

    #[tokio::test]
    async fn powershell_executes_commands_and_receives_resize() -> Result<()> {
        check_shell(
            LocalShell::PowerShell,
            b"> ",
            b"[Console]::WriteLine('cap'+'term-powershell-'+[Console]::WindowWidth)\rexit 7\r",
            b"capterm-powershell-103",
            7,
        )
        .await
    }

    #[tokio::test]
    async fn cmd_executes_commands_and_reports_exit_259() -> Result<()> {
        check_shell(
            LocalShell::CommandPrompt,
            b">",
            b"set capterm_probe=term-cmd\recho cap%capterm_probe%\rexit 259\r",
            b"capterm-cmd",
            259,
        )
        .await
    }

    #[tokio::test]
    async fn dropping_command_sender_closes_session_during_pending_input() -> Result<()> {
        timeout(Duration::from_secs(10), async {
            let (commands, mut output) = open(LocalShell::CommandPrompt, 80, 25).await?;
            commands.send(ShellCommand::Input(vec![b'x'; 2 * 1024 * 1024]))?;
            drop(commands);
            while let Some(event) = output.recv().await {
                match event {
                    ShellEvent::Output(_) => {}
                    ShellEvent::Exited(None) => return Ok(()),
                    ShellEvent::Exited(Some(code)) => bail!("Unexpected exit status {code}"),
                    ShellEvent::Error(error) => bail!("{error}"),
                }
            }
            bail!("Local shell output closed without an exit event")
        })
        .await
        .context("Closing local shell timed out")?
    }

    #[tokio::test]
    #[ignore = "requires an installed default WSL distribution"]
    async fn wsl_executes_commands_and_receives_resize() -> Result<()> {
        check_shell(
            LocalShell::Wsl,
            b"$ ",
            b"printf '%s%s-' cap term-wsl; stty size; exit 8\r",
            b"capterm-wsl-33 103",
            8,
        )
        .await
    }
}
