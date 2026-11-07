use std::{
    ffi::c_void,
    fs::File,
    future::Future,
    io::{Read, Seek, SeekFrom},
    mem::{ManuallyDrop, size_of, size_of_val},
    path::PathBuf,
    pin::Pin,
    ptr,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use tempfile::TempDir;
use tokio::{runtime::Handle, task::AbortHandle};
use windows::{
    Win32::{
        Foundation::{
            DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
            DV_E_DVASPECT, DV_E_DVTARGETDEVICE, DV_E_FORMATETC, DV_E_LINDEX, DV_E_TYMED, E_ABORT,
            E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, ERROR_CANCELLED, OLE_E_ADVISENOTSUPPORTED,
            S_FALSE, S_OK, STG_E_ACCESSDENIED, STG_E_INVALIDFUNCTION, STG_E_MEDIUMFULL,
            STG_E_READFAULT,
        },
        System::{
            Com::{
                CO_MTA_USAGE_COOKIE, COINIT_MULTITHREADED, CoDecrementMTAUsage,
                CoIncrementMTAUsage, CoInitializeEx, CoTaskMemAlloc, CoUninitialize, DATADIR_GET,
                DVASPECT_CONTENT, FORMATETC, IAdviseSink, IBindCtx, IDataObject, IDataObject_Impl,
                IEnumFORMATETC, IEnumSTATDATA, ISequentialStream_Impl, IStream, IStream_Impl,
                LOCKTYPE, Marshal::CoMarshalInterThreadInterfaceInStream, STATFLAG,
                STATFLAG_NONAME, STATSTG, STGC, STGM_READ, STGMEDIUM, STGMEDIUM_0, STGTY_STREAM,
                STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET,
                StructuredStorage::CoGetInterfaceAndReleaseStream, TYMED_HGLOBAL, TYMED_ISTREAM,
            },
            DataExchange::RegisterClipboardFormatW,
            Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::{
                DROPEFFECT, DROPEFFECT_COPY, DoDragDrop, IDropSource, IDropSource_Impl,
                OleInitialize, OleUninitialize, ReleaseStgMedium,
            },
            SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
        },
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, GetCapture, ReleaseCapture, VK_LBUTTON, VK_RBUTTON,
            },
            Shell::{
                CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, CFSTR_INDRAGLOOP,
                COPYENGINE_E_CANCELLED, COPYENGINE_E_USER_CANCELLED, FD_FILESIZE, FD_PROGRESSUI,
                FD_UNICODE, FILEDESCRIPTORW, FILEGROUPDESCRIPTORW, IDataObjectAsyncCapability,
                IDataObjectAsyncCapability_Impl, SHCreateStdEnumFmtEtc,
            },
            WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON},
        },
    },
    core::{BOOL, Error, HRESULT, Interface, PWSTR, Ref, implement},
};

type ComResult<T> = windows::core::Result<T>;
type Download =
    Box<dyn FnOnce(PathBuf) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + 'static>;
type Notify = Box<dyn FnOnce(Result<()>) + Send + 'static>;
const E_PENDING: HRESULT = HRESULT(0x8000_000a_u32 as i32);

struct OleApartment;

impl OleApartment {
    fn enter() -> Result<Self> {
        // GPUI already initializes OLE. S_FALSE still requires OleUninitialize.
        unsafe { OleInitialize(None) }.context("initialize OLE for file drag")?;
        Ok(Self)
    }
}

impl Drop for OleApartment {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
    }
}

struct MtaUsage(CO_MTA_USAGE_COOKIE);

// COM permits decrementing this usage cookie from a different thread. It is
// created once and released once, after all data objects and streams are gone.
unsafe impl Send for MtaUsage {}
unsafe impl Sync for MtaUsage {}

impl Drop for MtaUsage {
    fn drop(&mut self) {
        let _ = unsafe { CoDecrementMTAUsage(self.0) };
    }
}

struct MtaApartment;

impl Drop for MtaApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct MarshaledData(IStream);

// CoMarshalInterThreadInterfaceInStream returns a thread-safe stream for
// exactly this handoff. Its contained COM reference is still marshaled.
unsafe impl Send for MarshaledData {}

impl MarshaledData {
    fn unmarshal(self) -> ComResult<IDataObject> {
        // COM consumes the stream even when unmarshaling fails.
        let stream = ManuallyDrop::new(self.0);
        unsafe { CoGetInterfaceAndReleaseStream(&*stream) }
    }
}

struct Snapshot {
    file: Mutex<File>,
    size: u64,
    _directory: TempDir,
}

impl Snapshot {
    async fn download(
        download: Download,
        expected_size: Option<u64>,
    ) -> Result<Self, &'static str> {
        let directory = tempfile::Builder::new()
            .prefix("capterm-drag-")
            .tempdir()
            .map_err(|_| "Cannot create temporary download storage")?;
        let path = directory.path().join("contents");
        download(path.clone())
            .await
            .map_err(|_| "Could not download the remote file")?;
        let file = File::open(path).map_err(|_| "Cannot open the temporary download")?;
        let size = file
            .metadata()
            .map_err(|_| "Cannot read the temporary download size")?
            .len();
        if expected_size.is_some_and(|expected| size != expected) {
            return Err("The remote file changed size. Refresh the file list and try again");
        }
        Ok(Self {
            file: Mutex::new(file),
            size,
            _directory: directory,
        })
    }
}

enum Contents {
    Waiting(Download),
    Fetching(AbortHandle),
    Ready(Arc<Snapshot>),
    Failed(&'static str),
    Closed,
}

struct DragState {
    contents: Contents,
    released: bool,
    async_mode: bool,
    in_operation: bool,
    notify: Option<Notify>,
}

struct Transfer {
    runtime: Handle,
    state: Mutex<DragState>,
    ready: Condvar,
    size: Option<u64>,
    name: Vec<u16>,
    _mta: MtaUsage,
}

impl Transfer {
    fn new(
        name: Vec<u16>,
        size: Option<u64>,
        runtime: Handle,
        download: Download,
        notify: Notify,
    ) -> Result<Arc<Self>> {
        let mta = unsafe { CoIncrementMTAUsage() }.context("start Windows file data services")?;
        Ok(Arc::new(Self {
            runtime,
            size,
            name,
            state: Mutex::new(DragState {
                contents: Contents::Waiting(download),
                released: false,
                async_mode: true,
                in_operation: false,
                notify: Some(notify),
            }),
            ready: Condvar::new(),
            _mta: MtaUsage(mta),
        }))
    }

    fn snapshot(self: &Arc<Self>) -> ComResult<Arc<Snapshot>> {
        let mut state = self.state.lock().unwrap();
        if !state.released {
            // Targets can probe a stream during hover. Never start SFTP there.
            return Err(E_PENDING.into());
        }
        if matches!(&state.contents, Contents::Waiting(_)) {
            let Contents::Waiting(download) =
                std::mem::replace(&mut state.contents, Contents::Closed)
            else {
                unreachable!()
            };
            let transfer = Arc::downgrade(self);
            let size = self.size;
            let task = self.runtime.spawn(async move {
                let result = Snapshot::download(download, size).await;
                if let Some(transfer) = transfer.upgrade() {
                    transfer.download_finished(result);
                }
            });
            state.contents = Contents::Fetching(task.abort_handle());
        }
        loop {
            match &state.contents {
                Contents::Ready(snapshot) => return Ok(snapshot.clone()),
                Contents::Failed(_) => return Err(STG_E_READFAULT.into()),
                Contents::Closed => return Err(E_ABORT.into()),
                Contents::Fetching(task) if task.is_finished() => {
                    state.contents = Contents::Failed("The remote download was interrupted");
                }
                Contents::Fetching(_) => {
                    // Check for a stopped runtime or a panic before it can publish
                    // a result. Normally the worker notifies us immediately.
                    (state, _) = self
                        .ready
                        .wait_timeout(state, Duration::from_millis(250))
                        .unwrap();
                }
                Contents::Waiting(_) => unreachable!(),
            }
        }
    }

    fn download_finished(&self, result: Result<Snapshot, &'static str>) {
        let mut state = self.state.lock().unwrap();
        if matches!(&state.contents, Contents::Fetching(_)) {
            state.contents = match result {
                Ok(snapshot) => Contents::Ready(Arc::new(snapshot)),
                Err(error) => Contents::Failed(error),
            };
            self.ready.notify_all();
        }
    }

    fn finish(&self, status: HRESULT, effect: u32, asynchronous: bool) {
        let (notify, result) = {
            let mut state = self.state.lock().unwrap();
            if !asynchronous && state.in_operation && status.is_ok() {
                return;
            }
            state.in_operation = false;
            let result = match std::mem::replace(&mut state.contents, Contents::Closed) {
                Contents::Fetching(abort) => {
                    abort.abort();
                    None
                }
                Contents::Failed(error) if !cancelled(status) => Some(Err(anyhow!(error))),
                Contents::Ready(_)
                    if status.is_ok()
                        && status != DRAGDROP_S_CANCEL
                        && effect & DROPEFFECT_COPY.0 != 0 =>
                {
                    Some(Ok(()))
                }
                Contents::Ready(_) if status.is_err() && !cancelled(status) => {
                    Some(Err(anyhow!("The drop destination could not save the file")))
                }
                _ => None,
            };
            self.ready.notify_all();
            (state.notify.take(), result)
        };
        if let (Some(notify), Some(result)) = (notify, result) {
            notify(result);
        }
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        if let Contents::Fetching(abort) = &self.state.get_mut().unwrap().contents {
            abort.abort();
        }
    }
}

fn cancelled(status: HRESULT) -> bool {
    matches!(
        status,
        E_ABORT | COPYENGINE_E_CANCELLED | COPYENGINE_E_USER_CANCELLED
    ) || status == HRESULT::from_win32(ERROR_CANCELLED.0)
}

#[implement(IDropSource)]
struct FileDropSource {
    transfer: Arc<Transfer>,
}

impl IDropSource_Impl for FileDropSource_Impl {
    fn QueryContinueDrag(&self, escape_pressed: BOOL, key_state: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape_pressed.as_bool() {
            DRAGDROP_S_CANCEL
        } else if key_state.0 & MK_LBUTTON.0 == 0 {
            self.transfer.state.lock().unwrap().released = true;
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[implement(IDataObject, IDataObjectAsyncCapability)]
struct VirtualFile {
    descriptor_format: u16,
    contents_format: u16,
    drag_loop_format: u16,
    transfer: Arc<Transfer>,
}

impl VirtualFile {
    fn new(transfer: Arc<Transfer>) -> Result<Self> {
        let descriptor_format = unsafe { RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW) } as u16;
        let contents_format = unsafe { RegisterClipboardFormatW(CFSTR_FILECONTENTS) } as u16;
        let drag_loop_format = unsafe { RegisterClipboardFormatW(CFSTR_INDRAGLOOP) } as u16;
        ensure!(
            descriptor_format != 0 && contents_format != 0 && drag_loop_format != 0,
            "register Windows virtual file formats"
        );
        Ok(Self {
            descriptor_format,
            contents_format,
            drag_loop_format,
            transfer,
        })
    }

    fn formats(&self) -> [FORMATETC; 3] {
        [
            format(self.descriptor_format, -1, TYMED_HGLOBAL.0 as u32),
            format(self.contents_format, 0, TYMED_ISTREAM.0 as u32),
            format(self.drag_loop_format, -1, TYMED_HGLOBAL.0 as u32),
        ]
    }

    fn descriptor(&self) -> ComResult<STGMEDIUM> {
        let mut name = [0; 260];
        name[..self.transfer.name.len()].copy_from_slice(&self.transfer.name);
        let mut descriptor = FILEDESCRIPTORW {
            dwFlags: (FD_UNICODE.0 | FD_PROGRESSUI.0) as u32,
            cFileName: name,
            ..Default::default()
        };
        if let Some(size) = self.transfer.size {
            descriptor.dwFlags |= FD_FILESIZE.0 as u32;
            descriptor.nFileSizeHigh = (size >> 32) as u32;
            descriptor.nFileSizeLow = size as u32;
        }
        global_medium(FILEGROUPDESCRIPTORW {
            cItems: 1,
            fgd: [descriptor],
        })
    }
}

fn global_medium<T: Copy>(value: T) -> ComResult<STGMEDIUM> {
    let global = unsafe { GlobalAlloc(GHND, size_of::<T>()) }?;
    let mut medium = STGMEDIUM {
        tymed: TYMED_HGLOBAL.0 as u32,
        u: STGMEDIUM_0 { hGlobal: global },
        ..Default::default()
    };
    let memory = unsafe { GlobalLock(global) }.cast::<T>();
    if memory.is_null() {
        let error = Error::from_thread();
        unsafe { ReleaseStgMedium(&mut medium) };
        return Err(error);
    }
    unsafe {
        memory.write(value);
        let _ = GlobalUnlock(global);
    }
    Ok(medium)
}

fn format(cf_format: u16, index: i32, medium: u32) -> FORMATETC {
    FORMATETC {
        cfFormat: cf_format,
        ptd: ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: index,
        tymed: medium,
    }
}

impl IDataObject_Impl for VirtualFile_Impl {
    fn GetData(&self, format: *const FORMATETC) -> ComResult<STGMEDIUM> {
        self.QueryGetData(format).ok()?;
        let format = unsafe { (*format).cfFormat };
        if format == self.descriptor_format {
            return self.descriptor();
        }
        let released = self.transfer.state.lock().unwrap().released;
        if format == self.drag_loop_format {
            return global_medium(u32::from(!released));
        }
        if released && self.transfer.size == Some(0) {
            // A target may create an empty file without reading its stream.
            self.transfer.snapshot()?;
        }
        let stream = FileStream::new(self.transfer.clone(), 0);
        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 {
                pstm: ManuallyDrop::new(Some(stream.into())),
            },
            ..Default::default()
        })
    }

    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        let Some(requested) = (unsafe { format.as_ref() }) else {
            return E_POINTER;
        };
        let Some(offered) = self
            .formats()
            .into_iter()
            .find(|offered| offered.cfFormat == requested.cfFormat)
        else {
            return DV_E_FORMATETC;
        };
        if !requested.ptd.is_null() {
            return DV_E_DVTARGETDEVICE;
        }
        if offered.dwAspect != requested.dwAspect {
            return DV_E_DVASPECT;
        }
        if offered.tymed & requested.tymed == 0 {
            return DV_E_TYMED;
        }
        // Some targets query with -1 before requesting the only item at 0.
        if requested.lindex != offered.lindex
            && !(requested.cfFormat == self.contents_format && requested.lindex == -1)
        {
            return DV_E_LINDEX;
        }
        S_OK
    }

    fn EnumFormatEtc(&self, direction: u32) -> ComResult<IEnumFORMATETC> {
        if direction == DATADIR_GET.0 as u32 {
            unsafe { SHCreateStdEnumFmtEtc(&self.formats()) }
        } else {
            Err(E_NOTIMPL.into())
        }
    }

    fn GetDataHere(&self, _format: *const FORMATETC, _medium: *mut STGMEDIUM) -> ComResult<()> {
        Err(DV_E_TYMED.into())
    }

    fn GetCanonicalFormatEtc(&self, _input: *const FORMATETC, output: *mut FORMATETC) -> HRESULT {
        if output.is_null() {
            return E_POINTER;
        }
        unsafe { output.write(FORMATETC::default()) };
        DATA_S_SAMEFORMATETC
    }

    fn SetData(
        &self,
        _format: *const FORMATETC,
        _medium: *const STGMEDIUM,
        _release: BOOL,
    ) -> ComResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn DAdvise(
        &self,
        _format: *const FORMATETC,
        _flags: u32,
        _sink: Ref<IAdviseSink>,
    ) -> ComResult<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _connection: u32) -> ComResult<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> ComResult<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

impl IDataObjectAsyncCapability_Impl for VirtualFile_Impl {
    fn SetAsyncMode(&self, asynchronous: BOOL) -> ComResult<()> {
        self.transfer.state.lock().unwrap().async_mode = asynchronous.as_bool();
        Ok(())
    }

    fn GetAsyncMode(&self) -> ComResult<BOOL> {
        Ok(self.transfer.state.lock().unwrap().async_mode.into())
    }

    fn StartOperation(&self, _reserved: Ref<IBindCtx>) -> ComResult<()> {
        self.transfer.state.lock().unwrap().in_operation = true;
        Ok(())
    }

    fn InOperation(&self) -> ComResult<BOOL> {
        Ok(self.transfer.state.lock().unwrap().in_operation.into())
    }

    fn EndOperation(
        &self,
        status: HRESULT,
        _reserved: Ref<IBindCtx>,
        effect: u32,
    ) -> ComResult<()> {
        self.transfer.finish(status, effect, true);
        Ok(())
    }
}

#[implement(IStream)]
struct FileStream {
    transfer: Arc<Transfer>,
    position: Mutex<u64>,
}

impl FileStream {
    fn new(transfer: Arc<Transfer>, position: u64) -> Self {
        Self {
            transfer,
            position: Mutex::new(position),
        }
    }

    fn read(&self, mut buffer: &mut [u8]) -> ComResult<usize> {
        let snapshot = self.transfer.snapshot()?;
        let mut position = self.position.lock().unwrap();
        let mut file = snapshot.file.lock().unwrap();
        file.seek(SeekFrom::Start(*position))
            .map_err(|_| Error::from(STG_E_READFAULT))?;
        let mut total = 0;
        while !buffer.is_empty() {
            match file.read(buffer) {
                Ok(0) => break,
                Ok(count) => {
                    *position += count as u64;
                    total += count;
                    buffer = &mut buffer[count..];
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(STG_E_READFAULT.into()),
            }
        }
        Ok(total)
    }

    fn size(&self) -> ComResult<u64> {
        if !self.transfer.state.lock().unwrap().released {
            return self.transfer.size.ok_or_else(|| E_PENDING.into());
        }
        Ok(self.transfer.snapshot()?.size)
    }
}

impl ISequentialStream_Impl for FileStream_Impl {
    fn Read(&self, buffer: *mut c_void, length: u32, read: *mut u32) -> HRESULT {
        if !read.is_null() {
            unsafe { read.write(0) };
        }
        if length == 0 {
            return S_OK;
        }
        if buffer.is_null() || length as usize > isize::MAX as usize {
            return E_POINTER;
        }
        // The caller's output buffer may be uninitialized. Initialize it
        // before exposing it to Rust's Read as a byte slice.
        let buffer = unsafe {
            let bytes = buffer.cast::<u8>();
            bytes.write_bytes(0, length as usize);
            std::slice::from_raw_parts_mut(bytes, length as usize)
        };
        let count = match self.read(buffer) {
            Ok(count) => count as u32,
            Err(error) => return error.code(),
        };
        if !read.is_null() {
            unsafe { read.write(count) };
        }
        if count == length { S_OK } else { S_FALSE }
    }

    fn Write(&self, _buffer: *const c_void, _length: u32, written: *mut u32) -> HRESULT {
        if !written.is_null() {
            unsafe { written.write(0) };
        }
        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for FileStream_Impl {
    fn Seek(&self, offset: i64, origin: STREAM_SEEK, new_position: *mut u64) -> ComResult<()> {
        let end = if origin == STREAM_SEEK_END {
            Some(self.size()?)
        } else {
            None
        };
        let mut position = self.position.lock().unwrap();
        let base = match origin {
            STREAM_SEEK_SET => 0,
            STREAM_SEEK_CUR => *position,
            STREAM_SEEK_END => end.unwrap(),
            _ => return Err(STG_E_INVALIDFUNCTION.into()),
        };
        *position = base
            .checked_add_signed(offset)
            .ok_or_else(|| Error::from(STG_E_INVALIDFUNCTION))?;
        if !new_position.is_null() {
            unsafe { new_position.write(*position) };
        }
        Ok(())
    }

    fn CopyTo(
        &self,
        target: Ref<IStream>,
        length: u64,
        read: *mut u64,
        written: *mut u64,
    ) -> ComResult<()> {
        let target = target.ok()?;
        let mut total_read = 0;
        let mut total_written = 0;
        let mut buffer = vec![0; 64 * 1024];
        let result = (|| {
            // Even an empty file needs a validated remote snapshot.
            self.transfer.snapshot()?;
            while total_read < length {
                let request = (length - total_read).min(buffer.len() as u64) as usize;
                let count = self.read(&mut buffer[..request])?;
                if count == 0 {
                    break;
                }
                total_read += count as u64;
                let mut wrote = 0;
                let status =
                    unsafe { target.Write(buffer.as_ptr().cast(), count as u32, Some(&mut wrote)) };
                total_written += u64::from(wrote);
                status.ok()?;
                if wrote as usize != count {
                    return Err(STG_E_MEDIUMFULL.into());
                }
            }
            Ok(())
        })();
        unsafe {
            if !read.is_null() {
                read.write(total_read);
            }
            if !written.is_null() {
                written.write(total_written);
            }
        }
        result
    }

    fn Stat(&self, stat: *mut STATSTG, flags: &STATFLAG) -> ComResult<()> {
        if stat.is_null() {
            return Err(E_POINTER.into());
        }
        let size = self.size()?;
        let mut info = STATSTG {
            r#type: STGTY_STREAM.0 as u32,
            cbSize: size,
            grfMode: STGM_READ,
            ..Default::default()
        };
        if *flags != STATFLAG_NONAME {
            let name = &self.transfer.name;
            let bytes = size_of_val(name.as_slice()) + size_of::<u16>();
            let memory = unsafe { CoTaskMemAlloc(bytes) }.cast::<u16>();
            if memory.is_null() {
                return Err(E_OUTOFMEMORY.into());
            }
            unsafe {
                ptr::copy_nonoverlapping(name.as_ptr(), memory, name.len());
                memory.add(name.len()).write(0);
            }
            info.pwcsName = PWSTR(memory);
        }
        unsafe { stat.write(info) };
        Ok(())
    }

    fn Clone(&self) -> ComResult<IStream> {
        Ok(FileStream::new(self.transfer.clone(), *self.position.lock().unwrap()).into())
    }

    fn SetSize(&self, _size: u64) -> ComResult<()> {
        Err(STG_E_ACCESSDENIED.into())
    }

    fn Commit(&self, _flags: &STGC) -> ComResult<()> {
        Ok(())
    }

    fn Revert(&self) -> ComResult<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn LockRegion(&self, _offset: u64, _length: u64, _kind: &LOCKTYPE) -> ComResult<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn UnlockRegion(&self, _offset: u64, _length: u64, _kind: u32) -> ComResult<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }
}

fn data_object(transfer: Arc<Transfer>) -> Result<IDataObject> {
    // Explorer can fetch the stream asynchronously from another process. Host
    // the COM objects in the MTA so their blocking reads never run on GPUI's STA.
    let data = VirtualFile::new(transfer)?;
    let marshaled = thread::Builder::new()
        .name("capterm-ole-data".into())
        .spawn(move || {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
                .context("initialize Windows file data apartment")?;
            let _apartment = MtaApartment;
            let data: IDataObject = data.into();
            unsafe { CoMarshalInterThreadInterfaceInStream(&IDataObject::IID, &data) }
                .map(MarshaledData)
                .context("marshal Windows file data")
        })
        .context("start Windows file data apartment")?
        .join()
        .map_err(|_| anyhow!("Windows file data apartment stopped"))??;
    marshaled.unmarshal().context("open Windows file data")
}

fn primary_button_down() -> bool {
    // GetAsyncKeyState uses physical buttons, even when OLE and GPUI report
    // a logical primary button on a machine with swapped buttons.
    let button = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 {
        VK_RBUTTON
    } else {
        VK_LBUTTON
    };
    unsafe { GetAsyncKeyState(i32::from(button.0)) < 0 }
}

pub(super) fn drag_file(
    name: &str,
    size: Option<u64>,
    runtime: Handle,
    download: Download,
    notify: Notify,
) -> Result<()> {
    if !primary_button_down() {
        return Ok(());
    }
    let wide_name: Vec<u16> = name.encode_utf16().collect();
    ensure!(
        crate::ssh::valid_local_name(name) && wide_name.len() <= 255,
        "This remote file name cannot be used on Windows"
    );
    let _apartment = OleApartment::enter()?;
    let transfer = Transfer::new(wide_name, size, runtime, download, notify)?;
    let data = data_object(transfer.clone())?;
    // Verify that OLE's standard proxy can expose asynchronous extraction to
    // Explorer before making the virtual data available.
    let _: IDataObjectAsyncCapability = data.cast().context("enable asynchronous file drops")?;
    let source: IDropSource = FileDropSource {
        transfer: transfer.clone(),
    }
    .into();
    let mut effect = DROPEFFECT::default();

    if !primary_button_down() {
        return Ok(());
    }
    // GPUI takes capture at mouse-down. OLE takes over tracking.
    if !unsafe { GetCapture() }.is_invalid() {
        unsafe { ReleaseCapture() }.context("release GPUI mouse capture")?;
    }
    let status = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    transfer.finish(status, effect.0, false);
    status.ok().context("start Windows file drag")
}

#[cfg(test)]
mod tests;
