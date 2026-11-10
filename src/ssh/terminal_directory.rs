use super::Connection;

// The exec channel has its own shell and cwd. Find the single PTY session
// leader forked by the SSH worker that also started this probe. Read its cwd
// only while that shell owns the terminal, so a foreground job cannot make us
// follow a different process or fall back to the login directory.
const PROBE: &str = r#"sh -c '
worker=$PPID
for level in 1 2 3 4 5 6 7 8; do
    IFS= read -r name 2>/dev/null < "/proc/$worker/comm" || exit
    case $name in sshd|sshd-session|dropbear) break;; esac
    IFS= read -r stat 2>/dev/null < "/proc/$worker/stat" || exit
    set -- ${stat##*) }
    worker=$2
done
case $name in sshd|sshd-session|dropbear) ;; *) exit;; esac

children=/proc/$worker/task/$worker/children
if [ -r "$children" ]; then
    # The proc file has no trailing newline. read still assigns the PIDs.
    IFS= read -r pids 2>/dev/null < "$children"
    set -- $pids
else
    set -- /proc/[0-9]*
fi
set -f
leader=
for child do
    pid=${child#/proc/}
    IFS= read -r stat 2>/dev/null < "/proc/$pid/stat" || continue
    # Fields after the command name: state, ppid, pgrp, session, tty_nr, tpgid.
    set -- ${stat##*) }
    [ "$2" = "$worker" ] && [ "$4" = "$pid" ] && [ "$5" != 0 ] || continue
    [ -z "$leader" ] || exit
    leader=$pid
    foreground=$6
done
[ -n "$leader" ] && [ "$foreground" = "$leader" ] || exit

printf "\000capterm-cwd\000"
readlink -n "/proc/$leader/cwd" || exit
printf "\000"
'"#;

const TAG: &[u8] = b"\0capterm-cwd\0";

impl Connection {
    pub(crate) async fn terminal_directory(&self) -> Option<String> {
        let output = self.exec(PROBE).await.ok()?;
        Some(probe_path(&output)?.to_owned())
    }
}

fn probe_path(output: &[u8]) -> Option<&str> {
    let start = output.windows(TAG.len()).rposition(|part| part == TAG)? + TAG.len();
    let path = std::str::from_utf8(output[start..].strip_suffix(b"\0")?).ok()?;
    (path.starts_with('/') && !path.chars().any(char::is_control)).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::probe_path;

    #[test]
    fn accepts_only_a_tagged_absolute_pty_directory() {
        assert_eq!(
            probe_path("startup message\n\0capterm-cwd\0/tmp/中文 folder %\0".as_bytes()),
            Some("/tmp/中文 folder %")
        );
        assert_eq!(probe_path(b"/root\n"), None);
        assert_eq!(probe_path(b"\0capterm-cwd\0relative\0"), None);
        assert_eq!(probe_path(b"\0capterm-cwd\0/tmp/bad\n\0"), None);
        assert_eq!(probe_path(b"\0capterm-cwd\0/tmp/truncated"), None);
    }
}
