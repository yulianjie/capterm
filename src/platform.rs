use std::{future::Future, path::PathBuf};

use tokio::runtime::Handle;

pub const fn drag_supported() -> bool {
    cfg!(windows)
}

/// Offers a virtual file while the primary mouse button is held. The target
/// triggers the download only when it asks for contents after release.
/// Call on the UI thread with GPUI's state released for OLE's nested loop.
pub fn drag_file<F, Fut, N>(
    name: &str,
    size: Option<u64>,
    runtime: &Handle,
    download: F,
    notify: N,
) -> anyhow::Result<()>
where
    F: FnOnce(PathBuf) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    N: FnOnce(anyhow::Result<()>) + Send + 'static,
{
    #[cfg(windows)]
    {
        windows_drag::drag_file(
            name,
            size,
            runtime.clone(),
            Box::new(move |path| Box::pin(download(path))),
            Box::new(notify),
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (name, size, runtime, download, notify);
        anyhow::bail!("Dragging downloads out of the window is available on Windows")
    }
}

#[cfg(windows)]
mod windows_drag;
