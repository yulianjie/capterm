#[cfg(windows)]
mod text;

use std::fs;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, ensure};

use crate::ssh::RemoteEntry;

#[cfg(windows)]
pub(super) use text::{open_text, save_text};

#[cfg(not(windows))]
use crate::file_text::{FileText, SaveTextResult, TextEncoding};

#[cfg(not(windows))]
pub(super) async fn open_text(_: &str, _: TextEncoding, _: u64) -> Result<Option<FileText>> {
    anyhow::bail!("本地文件编辑仅适用于 Windows")
}

#[cfg(not(windows))]
pub(super) async fn save_text(_: &str, _: &FileText, _: &str, _: bool) -> Result<SaveTextResult> {
    anyhow::bail!("本地文件编辑仅适用于 Windows")
}

pub(super) async fn list(directory: &str) -> Result<(String, Vec<RemoteEntry>)> {
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || read_directory(&directory))
        .await
        .context("本地目录读取任务中断")?
}

fn read_directory(directory: &str) -> Result<(String, Vec<RemoteEntry>)> {
    let directory = if directory == "." {
        match dirs::home_dir().filter(|path| path.is_absolute() && path.is_dir()) {
            Some(home) => home,
            None => std::env::current_dir().context("无法获取本地工作目录")?,
        }
    } else {
        PathBuf::from(directory)
    };
    ensure!(directory.is_absolute(), "本地目录必须使用绝对路径");
    let display = directory.to_str().context("本地目录路径无法显示")?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(&directory).with_context(|| format!("读取 {display}"))? {
        let entry = entry.with_context(|| format!("读取 {display} 中的文件"))?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(path) = entry.path().into_os_string().into_string() else {
            continue;
        };
        let metadata = entry.metadata().ok();
        let is_dir = metadata.as_ref().is_some_and(is_directory);
        let hidden = name.starts_with('.') || metadata.as_ref().is_some_and(is_hidden);
        let mtime = metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|time| u32::try_from(time.as_secs()).ok());
        entries.push(RemoteEntry {
            path,
            name,
            is_dir,
            size: metadata.as_ref().map(fs::Metadata::len),
            mtime,
            permissions: None,
            hidden,
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok((display.to_owned(), entries))
}

#[cfg(windows)]
fn is_directory(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    // Junctions and directory symlinks keep this attribute in the directory entry.
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
}

#[cfg(not(windows))]
fn is_directory(metadata: &fs::Metadata) -> bool {
    metadata.is_dir()
}

#[cfg(windows)]
fn is_hidden(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x02;
    metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn is_hidden(_: &fs::Metadata) -> bool {
    false
}

pub(super) fn prompt_directory(line: &str) -> Option<String> {
    let path = line.trim_end_matches(' ').strip_prefix("PS ")?;
    drive_directory(path.strip_suffix('>')?.trim_end_matches('>'))
}

pub(super) fn osc_directory(directory: &str) -> Option<String> {
    drive_directory(directory.strip_prefix('/').unwrap_or(directory))
}

fn drive_directory(path: &str) -> Option<String> {
    let [drive, b':', b'\\' | b'/', ..] = path.as_bytes() else {
        return None;
    };
    (drive.is_ascii_alphabetic() && !path.chars().any(char::is_control))
        .then(|| path.replace('/', "\\"))
}

#[cfg(all(test, windows))]
mod tests {
    use std::path::Path;

    use super::*;

    #[tokio::test]
    async fn lists_native_paths_and_metadata_without_canonicalizing() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        fs::create_dir(fixture.path().join("Folder"))?;
        fs::write(fixture.path().join("资料's.txt"), b"hello")?;
        fs::write(fixture.path().join(".hidden"), b"")?;
        let directory = fixture.path().join(".");
        let display = directory
            .to_str()
            .context("fixture path cannot be displayed")?;
        let (listed, entries) = list(display).await?;

        assert_eq!(listed, display);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "Folder");
        assert!(entries[0].is_dir);
        assert!(entries[1].hidden);
        assert_eq!(entries[2].name, "资料's.txt");
        assert_eq!(Path::new(&entries[2].path), directory.join("资料's.txt"));
        assert_eq!(entries[2].size, Some(5));
        assert!(entries[2].mtime.is_some());
        assert!(list("C:relative").await.is_err());
        Ok(())
    }

    #[test]
    fn follows_only_drive_rooted_powershell_prompts() {
        assert_eq!(
            prompt_directory(r"PS C:\Users\O'Connor>> ").as_deref(),
            Some(r"C:\Users\O'Connor")
        );
        for line in [
            r"PS HKLM:\Software> ",
            r"PS C:relative> ",
            r"PS \\server\share> ",
            "PS C:\\unsafe\tpath> ",
        ] {
            assert_eq!(prompt_directory(line), None, "{line:?}");
        }
    }

    #[test]
    fn accepts_only_drive_rooted_osc_directories() {
        assert_eq!(osc_directory("/D:/資料").as_deref(), Some(r"D:\資料"));
        assert_eq!(osc_directory("D:/").as_deref(), Some(r"D:\"));
        for path in ["//server/share", "/tmp", "C:relative", "C:/unsafe\npath"] {
            assert_eq!(osc_directory(path), None, "{path:?}");
        }
    }
}
