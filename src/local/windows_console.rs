use std::{
    fs,
    io::{ErrorKind, Write},
    os::windows::ffi::OsStrExt,
    path::Path,
    sync::OnceLock,
};

use anyhow::{Context, Result, ensure};
use windows::{
    Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    },
    core::PCWSTR,
};

const VERSION: &str = "1.25.260930003";
#[cfg(target_arch = "x86_64")]
const BINARIES: (&[u8], &[u8]) = (
    include_bytes!("../../assets/windows/conpty/x64/OpenConsole.exe"),
    include_bytes!("../../assets/windows/conpty/x64/conpty.dll"),
);
#[cfg(target_arch = "aarch64")]
const BINARIES: (&[u8], &[u8]) = (
    include_bytes!("../../assets/windows/conpty/arm64/OpenConsole.exe"),
    include_bytes!("../../assets/windows/conpty/arm64/conpty.dll"),
);
#[cfg(target_arch = "x86")]
const BINARIES: (&[u8], &[u8]) = (
    include_bytes!("../../assets/windows/conpty/x86/OpenConsole.exe"),
    include_bytes!("../../assets/windows/conpty/x86/conpty.dll"),
);

static LOADED: OnceLock<Result<(), String>> = OnceLock::new();

pub(super) fn prepare() -> Result<()> {
    LOADED
        .get_or_init(|| load().map_err(|error| format!("{error:#}")))
        .as_ref()
        .map(|()| ())
        .map_err(|error| anyhow::anyhow!("{error}"))
}

fn load() -> Result<()> {
    let directory = dirs::cache_dir()
        .context("找不到 Windows 本地应用数据目录")?
        .join("CapTerm")
        .join("conpty")
        .join(format!("{VERSION}-{}", std::env::consts::ARCH));
    fs::create_dir_all(&directory).context("创建 Windows 控制台组件缓存目录")?;
    let dll = directory.join("conpty.dll");
    stage(
        &directory.join("LICENSE"),
        include_bytes!("../../assets/windows/conpty/LICENSE"),
    )?;
    stage(&directory.join("OpenConsole.exe"), BINARIES.0)?;
    stage(&dll, BINARIES.1)?;

    // portable-pty looks up conpty.dll by name. Preload the verified copy by
    // absolute path so it uses the matching side-by-side OpenConsole host.
    let path = dll
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        LoadLibraryExW(
            PCWSTR(path.as_ptr()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    }
    .context("加载 CapTerm 内置的 Windows 控制台组件")?;
    Ok(())
}

fn stage(path: &Path, bytes: &[u8]) -> Result<()> {
    match fs::read(path) {
        Ok(existing) => {
            ensure!(
                existing == bytes,
                "Windows 控制台组件缓存已损坏：{}",
                path.display()
            );
            return Ok(());
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("读取 {}", path.display())),
    }

    let directory = path.parent().context("找不到组件缓存的父目录")?;
    let mut staged = tempfile::NamedTempFile::new_in(directory)?;
    staged.write_all(bytes)?;
    match staged.persist_noclobber(path) {
        Ok(_) => Ok(()),
        // Another CapTerm instance can populate the same versioned cache.
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
            ensure!(
                fs::read(path)? == bytes,
                "Windows 控制台组件缓存已损坏：{}",
                path.display()
            );
            Ok(())
        }
        Err(error) => Err(error.error).with_context(|| format!("写入 {}", path.display())),
    }
}
