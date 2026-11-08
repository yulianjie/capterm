use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, Prefix};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, ensure};
use windows::Win32::Foundation::{
    ERROR_SHARING_VIOLATION, ERROR_UNABLE_TO_MOVE_REPLACEMENT_2, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_HIDDEN,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_SYSTEM, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_TYPE_DISK, FILE_WRITE_DATA,
    GetFileInformationByHandle, GetFileType, MOVE_FILE_FLAGS, MoveFileExW, REPLACE_FILE_FLAGS,
    ReplaceFileW, SetFileAttributesW,
};
use windows::core::PCWSTR;

use crate::file_text::{
    FileText, MEBIBYTE, PREVIEW_BYTES, SaveTextResult, TEXT_PROBE_BYTES, TextEncoding, decode_text,
    decode_text_buffer, encode_text, preserve_line_endings, strip_bom,
};

static SAVE_LOCK: Mutex<()> = Mutex::new(());

pub(in super::super) async fn open_text(
    path: &str,
    encoding: TextEncoding,
    edit_limit_mb: u64,
) -> Result<Option<FileText>> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let edit_limit = edit_limit_mb.clamp(1, 50) * MEBIBYTE;
        read_path(Path::new(&path), encoding, edit_limit).with_context(|| format!("读取 {path}"))
    })
    .await
    .context("本地文件读取任务中断")?
}

pub(in super::super) async fn save_text(
    path: &str,
    original: &FileText,
    edited: &str,
    force: bool,
) -> Result<SaveTextResult> {
    let (path, original, edited) = (path.to_owned(), original.clone(), edited.to_owned());
    tokio::task::spawn_blocking(move || {
        save_path(Path::new(&path), &original, &edited, force)
            .with_context(|| format!("保存 {path}"))
    })
    .await
    .context("本地文件保存任务中断")?
}

fn read_path(path: &Path, encoding: TextEncoding, edit_limit: u64) -> Result<Option<FileText>> {
    validate_path(path)?;
    let Some((mut file, metadata)) = open_regular(path, false)? else {
        return Ok(None);
    };
    let mut text = read_text(&mut file, &metadata, encoding, edit_limit)?;
    if let Some(text) = &mut text
        && !text.readonly
    {
        // Keep WSL previews read-only to preserve Linux ownership and permissions.
        // Check write and delete access on other paths without changing the file.
        text.readonly = wsl_share(path)
            || OpenOptions::new()
                .access_mode(FILE_WRITE_DATA.0 | DELETE.0)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
                .open(path)
                .is_err_and(|error| {
                    error.kind() == ErrorKind::PermissionDenied
                        && error.raw_os_error() != Some(ERROR_SHARING_VIOLATION.0 as i32)
                });
    }
    Ok(text)
}

fn wsl_share(path: &Path) -> bool {
    matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(
        prefix.kind(), Prefix::UNC(server, _) | Prefix::VerbatimUNC(server, _)
            if server.eq_ignore_ascii_case("wsl$") || server.eq_ignore_ascii_case("wsl.localhost")
    ))
}

fn validate_path(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(
                prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(..) | Prefix::VerbatimUNC(..)
            ))
            && path.file_name().is_some_and(|name| !name
                .encode_wide()
                .any(|ch| ch == 0 || ch == u16::from(b':'))),
        "本地文件路径无效"
    );
    Ok(())
}

fn regular(metadata: &Metadata) -> bool {
    metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
}

fn open_regular(path: &Path, saving: bool) -> Result<Option<(File, Metadata)>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if regular(&metadata) => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    if saving {
        // Hold the handle through replacement to exclude writers. ReplaceFileW itself needs DELETE.
        options
            .write(true)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    // SAFETY: the handle is valid for the duration of this call.
    if unsafe { GetFileType(HANDLE(file.as_raw_handle())) } != FILE_TYPE_DISK {
        return Ok(None);
    }
    let metadata = file.metadata()?;
    Ok(regular(&metadata).then_some((file, metadata)))
}

fn read_text(
    file: &mut File,
    metadata: &Metadata,
    encoding: TextEncoding,
    edit_limit: u64,
) -> Result<Option<FileText>> {
    let mut data = Vec::with_capacity(metadata.len().min(TEXT_PROBE_BYTES as u64) as usize);
    let prefix = (&mut *file)
        .take(TEXT_PROBE_BYTES as u64)
        .read_to_end(&mut data)?;
    let encoding = if encoding == TextEncoding::Gbk {
        encoding
    } else {
        [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
        ]
        .into_iter()
        .find(|&candidate| strip_bom(&data, candidate).1)
        .unwrap_or(TextEncoding::Utf8)
    };
    let (probe, _) = strip_bom(&data, encoding);
    if decode_local(probe, encoding, prefix == TEXT_PROBE_BYTES).is_none() {
        return Ok(None);
    }

    let oversized = metadata.len() > edit_limit;
    let read_limit = if oversized { PREVIEW_BYTES } else { edit_limit };
    if prefix == TEXT_PROBE_BYTES {
        let budget = metadata.len().saturating_add(1).min(read_limit + 1);
        data.reserve_exact((budget as usize).saturating_sub(prefix));
        file.take(read_limit + 1 - prefix as u64)
            .read_to_end(&mut data)?;
    }
    let truncated = data.len() as u64 > read_limit;
    if truncated {
        data.truncate(PREVIEW_BYTES as usize);
    }
    Ok(decode_text_buffer(data, encoding, truncated)
        .filter(|(text, _)| is_local_text(text))
        .map(|(text, bom)| FileText {
            text,
            readonly: truncated || oversized || metadata.permissions().readonly(),
            truncated,
            encoding,
            size: Some(metadata.len()),
            mtime: modified_seconds(metadata),
            bom,
            edit_limit,
        }))
}

fn decode_local(data: &[u8], encoding: TextEncoding, truncated: bool) -> Option<String> {
    decode_text(data, encoding, truncated).filter(|text| is_local_text(text))
}

fn is_local_text(text: &str) -> bool {
    !text.bytes().any(|byte| {
        byte.is_ascii_control() && !matches!(byte, b'\t' | b'\r' | b'\n' | 0x0b | 0x0c | 0x1b)
    })
}

fn modified_seconds(metadata: &Metadata) -> Option<u32> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs()
        .try_into()
        .ok()
}

fn save_path(
    path: &Path,
    original: &FileText,
    edited: &str,
    force: bool,
) -> Result<SaveTextResult> {
    validate_path(path)?;
    ensure!(!original.readonly, "本地文件为只读");
    let edit_limit = original.edit_limit;
    let parent = path.parent().context("本地文件缺少父目录")?;
    let parent = fs::canonicalize(parent).context("定位本地文件目录")?;
    let path = parent.join(path.file_name().context("本地文件名无效")?);

    let _lock = SAVE_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("本地文件保存锁失效"))?;
    let Some((mut file, metadata)) = open_regular(&path, true)? else {
        ensure!(!force, "本地文件已删除或不再是普通文件");
        return Ok(SaveTextResult::Conflict(None));
    };
    let current = read_text(&mut file, &metadata, original.encoding, edit_limit)?;
    if !force
        && current.as_ref().is_none_or(|current| {
            current.readonly
                || current.size != original.size
                || current.mtime != original.mtime
                || current.text != original.text
                || current.encoding != original.encoding
                || current.bom != original.bom
        })
    {
        return Ok(SaveTextResult::Conflict(current));
    }
    ensure!(
        current.is_some_and(|current| !current.readonly),
        "本地文件已变为只读或不再是可编辑的文本"
    );

    let text = preserve_line_endings(&original.text, edited);
    let bytes = encode_text(&text, original.encoding, original.bom)?;
    ensure!(
        bytes.len() as u64 <= edit_limit,
        "文件超过 {} MB 编辑上限",
        edit_limit / MEBIBYTE
    );
    let mut staged = tempfile::Builder::new()
        .prefix(".capterm-edit-")
        .tempfile_in(&parent)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    let staged = staged.into_temp_path();

    if !replace_checked(&path, &parent, file, &metadata, &staged)? {
        return Ok(SaveTextResult::Conflict(read_path(
            &path,
            original.encoding,
            edit_limit,
        )?));
    }
    Ok(SaveTextResult::Saved(FileText {
        text,
        readonly: false,
        truncated: false,
        encoding: original.encoding,
        size: Some(bytes.len() as u64),
        mtime: fs::metadata(&path).ok().as_ref().and_then(modified_seconds),
        bom: original.bom,
        edit_limit,
    }))
}

fn replace_checked(
    path: &Path,
    parent: &Path,
    file: File,
    metadata: &Metadata,
    staged: &Path,
) -> Result<bool> {
    let staged_name = wide_path(staged);

    // The temporary bit is removed before the file becomes the edited document. ReplaceFileW
    // handles the original DACL, streams, creation time, encryption and compression.
    let attributes = metadata.file_attributes()
        & (FILE_ATTRIBUTE_ARCHIVE
            | FILE_ATTRIBUTE_HIDDEN
            | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
            | FILE_ATTRIBUTE_SYSTEM)
            .0;
    let attributes = if attributes == 0 {
        FILE_ATTRIBUTE_NORMAL
    } else {
        FILE_FLAGS_AND_ATTRIBUTES(attributes)
    };
    // SAFETY: staged_name is a valid, nul-terminated pathname.
    unsafe { SetFileAttributesW(PCWSTR(staged_name.as_ptr()), attributes) }?;

    // ReplaceFileW can move the original aside before reporting an error. Give that file a
    // recoverable name and release the write guard before cleaning up or restoring it.
    let backup = tempfile::Builder::new()
        .prefix(".capterm-recover-")
        .tempdir_in(parent)?;
    let backup_path = backup.path().join("original");
    let backup_name = wide_path(&backup_path);
    let target_name = wide_path(path);
    let Some((latest, latest_metadata)) = open_regular(path, false)? else {
        return Ok(false);
    };
    if identity(&file)? != identity(&latest)?
        || metadata.len() != latest_metadata.len()
        || metadata.last_write_time() != latest_metadata.last_write_time()
        || metadata.file_attributes() != latest_metadata.file_attributes()
    {
        return Ok(false);
    }
    drop(latest);
    // SAFETY: all three buffers contain nul-terminated pathnames and remain live for the call.
    let replaced = unsafe {
        ReplaceFileW(
            PCWSTR(target_name.as_ptr()),
            PCWSTR(staged_name.as_ptr()),
            PCWSTR(backup_name.as_ptr()),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
    };
    drop(file);
    if let Err(error) = replaced {
        if error.code() == ERROR_UNABLE_TO_MOVE_REPLACEMENT_2.to_hresult() {
            // SAFETY: both path buffers are valid, and the flags require the destination to be absent.
            let restored = unsafe {
                MoveFileExW(
                    PCWSTR(backup_name.as_ptr()),
                    PCWSTR(target_name.as_ptr()),
                    MOVE_FILE_FLAGS(0),
                )
            };
            if let Err(restore_error) = restored {
                let backup_path = backup.keep().join("original");
                return Err(error).with_context(|| {
                    format!(
                        "无法恢复原文件（{restore_error}），原文件保留在 {}",
                        backup_path.display()
                    )
                });
            }
        }
        return Err(error).context("替换本地文件");
    }
    Ok(true)
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn identity(file: &File) -> Result<(u32, u32, u32)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is valid and info is writable for the duration of the call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }?;
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_save_preserves_encoding_newlines_and_windows_metadata() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let path = fixture.path().join("资料.txt");
        let path = path.to_str().context("fixture path is invalid")?;
        let stream = format!("{path}:notes");
        fs::write(
            path,
            encode_text("甲\r\n乙\r\n", TextEncoding::Utf16Le, true)?,
        )?;
        fs::write(&stream, b"alternate stream")?;
        let name = wide_path(Path::new(path));
        // SAFETY: name contains a nul-terminated pathname and is live for the call.
        unsafe { SetFileAttributesW(PCWSTR(name.as_ptr()), FILE_ATTRIBUTE_HIDDEN) }?;
        let creation = fs::metadata(path)?.creation_time();

        let original = open_text(path, TextEncoding::Utf8, 5)
            .await?
            .context("missing text")?;
        assert_eq!(original.encoding, TextEncoding::Utf16Le);
        assert_eq!(original.text, "甲\r\n乙\r\n");
        assert!(!original.readonly);
        let SaveTextResult::Saved(saved) = save_text(path, &original, "甲\n改\n", false).await?
        else {
            anyhow::bail!("unexpected conflict");
        };

        assert_eq!(
            fs::read(path)?,
            encode_text("甲\r\n改\r\n", TextEncoding::Utf16Le, true)?
        );
        assert_eq!(fs::read(&stream)?, b"alternate stream");
        let after = fs::metadata(path)?;
        assert_eq!(after.creation_time(), creation);
        assert_ne!(after.file_attributes() & FILE_ATTRIBUTE_HIDDEN.0, 0);
        assert_eq!(saved.mtime, modified_seconds(&after));
        assert_eq!(fs::read_dir(fixture.path())?.count(), 1);

        fs::write(path, "外部修改\n")?;
        let SaveTextResult::Conflict(Some(current)) =
            save_text(path, &saved, "不覆盖\n", false).await?
        else {
            anyhow::bail!("expected an encoding change conflict");
        };
        assert_eq!(current.encoding, TextEncoding::Utf8);
        assert_eq!(current.text, "外部修改\n");
        Ok(())
    }

    #[tokio::test]
    async fn local_save_detects_same_size_same_time_changes_and_serializes_editors() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let path = fixture.path().join("test.txt");
        let path = path.to_str().context("fixture path is invalid")?;
        fs::write(path, b"one\n")?;
        let original = open_text(path, TextEncoding::Utf8, 5)
            .await?
            .context("missing text")?;

        let modified = fs::metadata(path)?.modified()?;
        fs::write(path, b"two\n")?;
        OpenOptions::new()
            .write(true)
            .open(path)?
            .set_modified(modified)?;
        let SaveTextResult::Conflict(Some(current)) =
            save_text(path, &original, "bad\n", false).await?
        else {
            anyhow::bail!("expected a conflict");
        };
        assert_eq!(current.mtime, original.mtime);
        assert_eq!(current.size, original.size);
        assert_eq!(current.text, "two\n");
        assert_eq!(fs::read(path)?, b"two\n");

        let (first, second) = tokio::join!(
            save_text(path, &current, "aaa\n", false),
            save_text(path, &current, "bbb\n", false),
        );
        match (first?, second?) {
            (SaveTextResult::Saved(saved), SaveTextResult::Conflict(Some(conflict)))
            | (SaveTextResult::Conflict(Some(conflict)), SaveTextResult::Saved(saved)) => {
                assert_eq!(conflict.text, saved.text);
                assert_eq!(fs::read(path)?, saved.text.as_bytes());
            }
            _ => anyhow::bail!("one concurrent save must succeed and the other must conflict"),
        }
        assert_eq!(fs::read_dir(fixture.path())?.count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn local_open_bounds_previews_and_distinguishes_readonly_and_binary() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let path = fixture.path().join("preview.txt");
        let path = path.to_str().context("fixture path is invalid")?;
        let mut data = vec![b'x'; PREVIEW_BYTES as usize - 1];
        data.extend_from_slice("中\n".as_bytes());
        fs::write(path, data)?;
        let size = 64 * MEBIBYTE;
        OpenOptions::new().write(true).open(path)?.set_len(size)?;
        let preview = open_text(path, TextEncoding::Utf8, 1)
            .await?
            .context("missing preview")?;
        assert!(preview.readonly && preview.truncated);
        assert_eq!(preview.size, Some(size));
        assert_eq!(preview.text.len(), PREVIEW_BYTES as usize - 1);
        assert!(save_text(path, &preview, "new", true).await.is_err());

        fs::write(path, [b'a', 0, b'b'])?;
        assert!(open_text(path, TextEncoding::Utf8, 5).await?.is_none());

        fs::write(path, b"readonly\n")?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions.clone())?;
        let read = open_text(path, TextEncoding::Utf8, 5).await;
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)?;
        let read = read?.context("missing readonly text")?;
        assert!(read.readonly && !read.truncated);
        assert_eq!(read.text, "readonly\n");
        Ok(())
    }
}
