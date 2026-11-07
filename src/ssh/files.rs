use std::fs::FileTimes;
use std::time::{SystemTime, UNIX_EPOCH};

use russh_sftp::client::error::Error as SftpError;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

use super::*;

const MEBIBYTE: u64 = 1024 * 1024;
const PREVIEW_LIMIT: u64 = 50 * 1024 * 1024;
const PREVIEW_BYTES: u64 = 1024 * 1024;
const TEXT_PROBE_BYTES: usize = 8192;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TextEncoding {
    #[default]
    Utf8,
    Gbk,
}

impl TextEncoding {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Gbk => "GBK",
        }
    }
}

#[derive(Clone)]
pub(crate) struct RemoteText {
    pub text: String,
    pub readonly: bool,
    pub encoding: TextEncoding,
    pub size: Option<u64>,
    pub mtime: Option<u32>,
    bom: bool,
    edit_limit: u64,
}

pub(crate) enum SaveTextResult {
    Saved(RemoteText),
    Conflict(Option<RemoteText>),
}

pub(crate) enum RemoteMutation {
    CreateFile(String),
    CreateDirectory(String),
    Rename { old: String, new: String },
    Delete { path: String, directory: bool },
    Chmod { path: String, mode: u32 },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TransferChoice {
    #[default]
    Ask,
    Overwrite,
    KeepBoth,
    Skip,
}

#[derive(Clone, Copy)]
pub(crate) struct TransferOptions {
    pub choice: TransferChoice,
    pub preserve_metadata: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct TransferConflict {
    pub size: Option<u64>,
    pub mtime: Option<u32>,
    pub directory: bool,
}

pub(crate) enum TransferOutcome {
    Complete(String),
    Conflict(TransferConflict),
    Cancelled,
    Skipped,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum TransferSignal {
    Running,
    Paused,
    Cancelled,
}

impl Connection {
    pub(crate) async fn prepare_clipboard_directory(&self, directory: &str) -> Result<()> {
        ensure!(directory == "/tmp/capterm", "剪贴板上传目录无效");
        let output = self
            .exec("umask 077; mkdir -p /tmp/capterm && id -u")
            .await?;
        let uid = std::str::from_utf8(&output)?.trim().parse::<u32>()?;
        let sftp = self.sftp().await?;
        let target = sftp.symlink_metadata(directory).await?;
        ensure!(
            target.file_type().is_dir()
                && target.uid == Some(uid)
                && target.permissions.is_some_and(|mode| mode & 0o077 == 0),
            "远端 /tmp/capterm 必须是当前 SSH 用户独有的目录（权限 0700）"
        );
        Ok(())
    }

    pub(crate) async fn open_remote_text(
        &self,
        path: &str,
        encoding: TextEncoding,
        edit_limit_mb: u64,
    ) -> Result<Option<RemoteText>> {
        let edit_limit = edit_limit_mb.clamp(1, 50) * MEBIBYTE;
        read_text(self.sftp().await?.as_ref(), path, encoding, edit_limit).await
    }

    pub(crate) async fn save_remote_text(
        &self,
        path: &str,
        original: &RemoteText,
        edited: &str,
        force: bool,
    ) -> Result<SaveTextResult> {
        let _guard = self.edit_lock.lock().await;
        let sftp = self.sftp().await?;
        let metadata = sftp.symlink_metadata(path).await?;
        ensure!(metadata.file_type().is_file(), "只能保存普通文件");
        let edit_limit = original.edit_limit;
        let current = read_text(&sftp, path, original.encoding, edit_limit).await?;
        if !force
            && current.as_ref().is_none_or(|current| {
                current.size != original.size
                    || current.mtime != original.mtime
                    || current.text != original.text
                    || current.bom != original.bom
            })
        {
            return Ok(SaveTextResult::Conflict(current));
        }

        let text = preserve_line_endings(&original.text, edited);
        let bytes = encode_text(&text, original.encoding, original.bom)?;
        ensure!(
            bytes.len() as u64 <= edit_limit,
            "文件超过 {} MB 编辑上限",
            edit_limit / MEBIBYTE
        );
        let mut staged = RemoteStage::create(sftp.clone(), remote_temp(path)).await?;
        staged.file().write_all(&bytes).await?;
        staged.finish_writing().await?;
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
        .context("保留远端权限和属主")?;
        let written = sftp.symlink_metadata(&staged.path).await?;
        let replacement = self.atomic_rename_channel().await?;
        let before_rename = sftp.symlink_metadata(path).await?;
        if !same_file(&metadata, &before_rename)
            || (!force
                && read_text(&sftp, path, original.encoding, edit_limit)
                    .await?
                    .is_none_or(|current| {
                        current.text != original.text || current.bom != original.bom
                    }))
        {
            return Ok(SaveTextResult::Conflict(
                read_text(&sftp, path, original.encoding, edit_limit).await?,
            ));
        }
        posix_rename(&replacement.0, &staged.path, path).await?;
        staged.commit();
        Ok(SaveTextResult::Saved(RemoteText {
            text,
            readonly: false,
            encoding: original.encoding,
            size: written.size,
            mtime: written.mtime,
            bom: original.bom,
            edit_limit,
        }))
    }

    async fn atomic_rename_channel(&self) -> Result<AtomicRenameChannel> {
        let channel = AtomicRenameChannel(RawSftpSession::new(
            self.sftp_channel().await?.into_stream(),
        ));
        let version = channel.0.init().await?;
        ensure!(
            version
                .extensions
                .get(POSIX_RENAME)
                .is_some_and(|version| version == "1"),
            "服务器不支持原子覆盖"
        );
        Ok(channel)
    }

    pub(crate) async fn mutate_remote(&self, operation: RemoteMutation) -> Result<()> {
        let sftp = self.sftp().await?;
        match operation {
            RemoteMutation::CreateFile(path) => {
                validate_path_name(&path)?;
                sftp.open_with_flags(
                    &path,
                    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
                )
                .await?
                .close()
                .await?;
            }
            RemoteMutation::CreateDirectory(path) => {
                validate_path_name(&path)?;
                sftp.create_dir(path).await?;
            }
            RemoteMutation::Rename { old, new } => {
                validate_path_name(&new)?;
                sftp.rename(old, new).await?;
            }
            RemoteMutation::Delete { path, directory } => {
                if directory {
                    sftp.remove_dir(path).await?;
                } else {
                    sftp.remove_file(path).await?;
                }
            }
            RemoteMutation::Chmod { path, mode } => {
                sftp.set_metadata(
                    path,
                    FileAttributes {
                        permissions: Some(mode & 0o7777),
                        ..FileAttributes::empty()
                    },
                )
                .await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn upload_file(
        &self,
        local: &Path,
        remote_dir: &str,
        options: TransferOptions,
        mut signal: watch::Receiver<TransferSignal>,
        progress: impl FnMut(u64, u64),
    ) -> Result<TransferOutcome> {
        if !transfer_permit(&mut signal).await {
            return Ok(TransferOutcome::Cancelled);
        }
        let choice = options.choice;
        let name = local
            .file_name()
            .and_then(|name| name.to_str())
            .context("需要 UTF-8 文件名")?;
        ensure!(valid_remote_name(name), "远端文件名无效");
        let mut source = File::open(local).await.context("打开本地文件")?;
        let metadata = source.metadata().await?;
        ensure!(metadata.is_file(), "只能上传普通文件");
        let sftp = self.sftp().await?;
        let directory = sftp.canonicalize(remote_dir).await?;
        let destination = join_remote(&directory, name);
        if let Some(conflict) = remote_conflict(&sftp, &destination).await? {
            if choice == TransferChoice::Skip {
                return Ok(TransferOutcome::Skipped);
            }
            if choice == TransferChoice::Ask
                || (conflict.directory && choice == TransferChoice::Overwrite)
            {
                return Ok(TransferOutcome::Conflict(conflict));
            }
        }
        let mut staged = RemoteStage::create(sftp.clone(), remote_temp(&destination)).await?;
        if !copy_chunks(
            &mut source,
            staged.file(),
            metadata.len(),
            &mut signal,
            progress,
        )
        .await?
        {
            return Ok(TransferOutcome::Cancelled);
        }
        staged.finish_writing().await?;
        if options.preserve_metadata {
            sftp.set_metadata(&staged.path, remote_upload_metadata(&metadata))
                .await?;
        }
        let Some(_guard) = commit_lock(&self.edit_lock, &mut signal).await else {
            return Ok(TransferOutcome::Cancelled);
        };
        let mut destination = destination;
        let mut suffix = 1;
        loop {
            if let Some(conflict) = remote_conflict(&sftp, &destination).await? {
                match choice {
                    TransferChoice::Ask => return Ok(TransferOutcome::Conflict(conflict)),
                    TransferChoice::Skip => return Ok(TransferOutcome::Skipped),
                    TransferChoice::Overwrite if conflict.directory => {
                        return Ok(TransferOutcome::Conflict(conflict));
                    }
                    TransferChoice::Overwrite => {
                        let channel = self.atomic_rename_channel().await?;
                        posix_rename(&channel.0, &staged.path, &destination).await?;
                        break;
                    }
                    TransferChoice::KeepBoth => {
                        destination = join_remote(&directory, &numbered_name(name, suffix));
                        suffix += 1;
                        continue;
                    }
                }
            }
            match sftp.rename(&staged.path, &destination).await {
                Ok(()) => break,
                Err(error) if remote_conflict(&sftp, &destination).await?.is_some() => {
                    let _ = error;
                }
                Err(error) => return Err(error.into()),
            }
        }
        staged.commit();
        Ok(TransferOutcome::Complete(destination))
    }

    pub(crate) async fn download_file(
        &self,
        remote: &str,
        local: &Path,
        options: TransferOptions,
        mut signal: watch::Receiver<TransferSignal>,
        progress: impl FnMut(u64, u64),
    ) -> Result<TransferOutcome> {
        if !transfer_permit(&mut signal).await {
            return Ok(TransferOutcome::Cancelled);
        }
        let choice = options.choice;
        let name = local
            .file_name()
            .and_then(|name| name.to_str())
            .context("需要 UTF-8 文件名")?;
        ensure!(valid_local_name(name), "本地文件名无效");
        let parent = local
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if let Some(conflict) = local_conflict(local).await? {
            if choice == TransferChoice::Skip {
                return Ok(TransferOutcome::Skipped);
            }
            if choice == TransferChoice::Ask
                || (conflict.directory && choice == TransferChoice::Overwrite)
            {
                return Ok(TransferOutcome::Conflict(conflict));
            }
        }
        let sftp = self.sftp().await?;
        let metadata = sftp.symlink_metadata(remote).await?;
        ensure!(metadata.file_type().is_file(), "只能下载普通文件");
        let mut source = sftp.open(remote).await?;
        let staged = tempfile::Builder::new()
            .prefix(".capterm-")
            .tempfile_in(parent)?;
        let (file, mut temp_path) = staged.into_parts();
        let mut file = File::from_std(file);
        let copied = copy_chunks(
            &mut source,
            &mut file,
            metadata.size.unwrap_or_default(),
            &mut signal,
            progress,
        )
        .await;
        let closed = source.close().await;
        if !copied? {
            return Ok(TransferOutcome::Cancelled);
        }
        closed?;
        file.sync_all().await?;
        let file = file.into_std().await;
        let Some(_guard) = commit_lock(&self.edit_lock, &mut signal).await else {
            return Ok(TransferOutcome::Cancelled);
        };
        if options.preserve_metadata {
            preserve_local_download_metadata(&file, &metadata)?;
        }
        drop(file);
        let mut destination = local.to_path_buf();
        let mut suffix = 1;
        loop {
            if let Some(conflict) = local_conflict(&destination).await? {
                match choice {
                    TransferChoice::Ask => return Ok(TransferOutcome::Conflict(conflict)),
                    TransferChoice::Skip => return Ok(TransferOutcome::Skipped),
                    TransferChoice::Overwrite if conflict.directory => {
                        return Ok(TransferOutcome::Conflict(conflict));
                    }
                    TransferChoice::Overwrite => {
                        temp_path.persist(&destination).context("替换本地文件")?;
                        break;
                    }
                    TransferChoice::KeepBoth => {
                        destination = parent.join(numbered_name(name, suffix));
                        suffix += 1;
                        continue;
                    }
                }
            }
            match temp_path.persist_noclobber(&destination) {
                Ok(()) => break,
                Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
                    temp_path = error.path
                }
                Err(error) => return Err(error).context("保存下载文件"),
            }
        }
        Ok(TransferOutcome::Complete(
            destination.to_string_lossy().into_owned(),
        ))
    }
}

struct AtomicRenameChannel(RawSftpSession);

impl Drop for AtomicRenameChannel {
    fn drop(&mut self) {
        let _ = self.0.close_session();
    }
}

fn same_file(before: &FileAttributes, after: &FileAttributes) -> bool {
    after.file_type().is_file()
        && before.size == after.size
        && before.mtime == after.mtime
        && before.permissions == after.permissions
        && before.uid == after.uid
        && before.gid == after.gid
}

async fn read_text(
    sftp: &SftpSession,
    path: &str,
    encoding: TextEncoding,
    edit_limit: u64,
) -> Result<Option<RemoteText>> {
    let metadata = sftp.symlink_metadata(path).await?;
    if !metadata.file_type().is_file() || metadata.size.is_some_and(|size| size > PREVIEW_LIMIT) {
        return Ok(None);
    }
    let readonly = metadata.size.is_some_and(|size| size > edit_limit);
    let limit = if readonly {
        PREVIEW_BYTES
    } else {
        edit_limit + 1
    };
    let mut file = sftp.open(path).await?;
    let mut data = Vec::new();
    let read = async {
        let prefix = (&mut file)
            .take(TEXT_PROBE_BYTES as u64)
            .read_to_end(&mut data)
            .await?;
        if data.contains(&0) || decode_text(&data, encoding, prefix == TEXT_PROBE_BYTES).is_none() {
            return Ok::<_, std::io::Error>(false);
        }
        (&mut file)
            .take(limit.saturating_sub(prefix as u64))
            .read_to_end(&mut data)
            .await?;
        Ok(true)
    }
    .await;
    let closed = file.close().await;
    let text = read?;
    closed?;
    if !text {
        return Ok(None);
    }
    let readonly = readonly || data.len() as u64 > edit_limit;
    if readonly {
        data.truncate(PREVIEW_BYTES as usize);
    }
    let bom = encoding == TextEncoding::Utf8 && data.starts_with(b"\xef\xbb\xbf");
    let data = if bom { &data[3..] } else { &data };
    let Some(text) = decode_text(data, encoding, readonly) else {
        return Ok(None);
    };
    Ok(Some(RemoteText {
        text,
        readonly,
        encoding,
        size: metadata.size,
        mtime: metadata.mtime,
        bom,
        edit_limit,
    }))
}

fn decode_text(data: &[u8], encoding: TextEncoding, truncated: bool) -> Option<String> {
    match encoding {
        TextEncoding::Utf8 => match std::str::from_utf8(data) {
            Ok(text) => Some(text.into()),
            Err(error) if truncated && error.error_len().is_none() => {
                std::str::from_utf8(&data[..error.valid_up_to()])
                    .ok()
                    .map(str::to_owned)
            }
            Err(_) => None,
        },
        TextEncoding::Gbk => encoding_rs::GBK
            .decode_without_bom_handling_and_without_replacement(data)
            .or_else(|| {
                truncated
                    .then(|| data.split_last())
                    .flatten()
                    .and_then(|(_, data)| {
                        encoding_rs::GBK.decode_without_bom_handling_and_without_replacement(data)
                    })
            })
            .map(|text| text.into_owned()),
    }
}

fn encode_text(text: &str, encoding: TextEncoding, bom: bool) -> Result<Vec<u8>> {
    match encoding {
        TextEncoding::Utf8 if bom => {
            let mut bytes = b"\xef\xbb\xbf".to_vec();
            bytes.extend_from_slice(text.as_bytes());
            Ok(bytes)
        }
        TextEncoding::Utf8 => Ok(text.as_bytes().to_vec()),
        TextEncoding::Gbk => {
            let (bytes, _, had_errors) = encoding_rs::GBK.encode(text);
            ensure!(!had_errors, "编辑内容包含 GBK 无法表示的字符");
            Ok(bytes.into_owned())
        }
    }
}

fn validate_path_name(path: &str) -> Result<()> {
    ensure!(
        path.rsplit('/').next().is_some_and(valid_remote_name),
        "远端文件名无效"
    );
    Ok(())
}

async fn remote_conflict(sftp: &SftpSession, path: &str) -> Result<Option<TransferConflict>> {
    match sftp.symlink_metadata(path).await {
        Ok(metadata) => Ok(Some(TransferConflict {
            size: metadata.size,
            mtime: metadata.mtime,
            directory: metadata.file_type().is_dir(),
        })),
        Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn local_conflict(path: &Path) -> Result<Option<TransferConflict>> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) => Ok(Some(TransferConflict {
            size: Some(metadata.len()),
            mtime: metadata.modified().ok().and_then(unix_seconds),
            directory: metadata.is_dir(),
        })),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn unix_seconds(time: SystemTime) -> Option<u32> {
    time.duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .try_into()
        .ok()
}

fn remote_upload_metadata(metadata: &std::fs::Metadata) -> FileAttributes {
    let mut attributes = FileAttributes::empty();
    if let (Some(atime), Some(mtime)) = (
        metadata.accessed().ok().and_then(unix_seconds),
        metadata.modified().ok().and_then(unix_seconds),
    ) {
        attributes.atime = Some(atime);
        attributes.mtime = Some(mtime);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        attributes.permissions = Some(metadata.permissions().mode() & 0o7777);
    }
    #[cfg(not(unix))]
    {
        attributes.permissions = Some(if metadata.permissions().readonly() {
            0o444
        } else {
            0o644
        });
    }
    attributes
}

fn preserve_local_download_metadata(file: &std::fs::File, metadata: &FileAttributes) -> Result<()> {
    let mut times = FileTimes::new();
    if let Some(atime) = metadata.atime {
        times = times.set_accessed(UNIX_EPOCH + Duration::from_secs(u64::from(atime)));
    }
    if let Some(mtime) = metadata.mtime {
        times = times.set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(mtime)));
    }
    file.set_times(times)?;
    if let Some(mode) = metadata.permissions {
        let mut permissions = file.metadata()?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(mode & 0o777);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(mode & 0o222 == 0);
        file.set_permissions(permissions)?;
    }
    Ok(())
}

fn numbered_name(name: &str, suffix: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            format!("{stem} ({suffix}).{extension}")
        }
        _ => format!("{name} ({suffix})"),
    }
}

async fn transfer_permit(signal: &mut watch::Receiver<TransferSignal>) -> bool {
    signal
        .wait_for(|signal| *signal != TransferSignal::Paused)
        .await
        .is_ok_and(|signal| *signal == TransferSignal::Running)
}

async fn commit_lock<'a>(
    lock: &'a Mutex<()>,
    signal: &mut watch::Receiver<TransferSignal>,
) -> Option<tokio::sync::MutexGuard<'a, ()>> {
    while transfer_permit(signal).await {
        let guard = lock.lock().await;
        if *signal.borrow() == TransferSignal::Running {
            return Some(guard);
        }
    }
    None
}

async fn copy_chunks(
    source: &mut (impl AsyncRead + Unpin),
    destination: &mut (impl AsyncWrite + Unpin),
    total: u64,
    signal: &mut watch::Receiver<TransferSignal>,
    mut progress: impl FnMut(u64, u64),
) -> Result<bool> {
    let mut buffer = vec![0; 64 * 1024];
    let mut copied = 0;
    progress(copied, total);
    loop {
        if !transfer_permit(signal).await {
            return Ok(false);
        }
        let read = source.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        destination.write_all(&buffer[..read]).await?;
        copied += read as u64;
        progress(copied, total);
    }
    Ok(true)
}
