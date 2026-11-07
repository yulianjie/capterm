use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc::{Receiver, channel},
};

use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;

use super::*;

fn setup(
    size: Option<u64>,
    download: Download,
) -> (
    OleApartment,
    tokio::runtime::Runtime,
    Arc<Transfer>,
    Receiver<Result<()>>,
) {
    let ole = OleApartment::enter().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (notify, notices) = channel();
    let transfer = Transfer::new(
        "远程-é.txt".encode_utf16().collect(),
        size,
        runtime.handle().clone(),
        download,
        Box::new(move |result| {
            notify.send(result).unwrap();
        }),
    )
    .unwrap();
    (ole, runtime, transfer, notices)
}

fn stream(data: &IDataObject) -> IStream {
    let contents_format = unsafe { RegisterClipboardFormatW(CFSTR_FILECONTENTS) };
    let mut medium =
        unsafe { data.GetData(&format(contents_format as u16, 0, TYMED_ISTREAM.0 as u32)) }
            .unwrap();
    let stream = unsafe { medium.u.pstm.as_ref().unwrap().clone() };
    unsafe { ReleaseStgMedium(&mut medium) };
    stream
}

#[test]
fn virtual_file_is_lazy_and_copies_through_the_com_proxy() {
    let requests = Arc::new(AtomicUsize::new(0));
    let worker_requests = requests.clone();
    let (_ole, _runtime, transfer, notices) = setup(
        Some(8),
        Box::new(move |path| {
            Box::pin(async move {
                worker_requests.fetch_add(1, Ordering::SeqCst);
                tokio::fs::write(path, b"SFTP\0OLE").await?;
                Ok(())
            })
        }),
    );
    let data = data_object(transfer.clone()).unwrap();
    let descriptor_format = unsafe { RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW) };
    let mut descriptor = unsafe {
        data.GetData(&format(
            descriptor_format as u16,
            -1,
            TYMED_HGLOBAL.0 as u32,
        ))
    }
    .unwrap();
    unsafe {
        let global = descriptor.u.hGlobal;
        let group = GlobalLock(global).cast::<FILEGROUPDESCRIPTORW>().read();
        let count = group.cItems;
        let name = group.fgd[0].cFileName;
        assert_eq!(count, 1);
        assert_eq!(&name[..transfer.name.len()], transfer.name);
        assert_eq!(name[transfer.name.len()], 0);
        let _ = GlobalUnlock(global);
        ReleaseStgMedium(&mut descriptor);
    }

    let file = stream(&data);
    let mut hover = [0u8; 1];
    let status = unsafe { file.Read(hover.as_mut_ptr().cast(), 1, None) };
    assert_eq!(status, E_PENDING);
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(notices.try_recv().is_err());

    let source: IDropSource = FileDropSource {
        transfer: transfer.clone(),
    }
    .into();
    assert_eq!(
        unsafe { source.QueryContinueDrag(false, MODIFIERKEYS_FLAGS(0)) },
        DRAGDROP_S_DROP
    );
    let asynchronous = data.cast::<IDataObjectAsyncCapability>().unwrap();
    unsafe {
        assert!(asynchronous.GetAsyncMode().unwrap().as_bool());
        asynchronous.StartOperation(None::<&IBindCtx>).unwrap();
    }
    transfer.finish(DRAGDROP_S_DROP, DROPEFFECT_COPY.0, false);

    let target = unsafe { CreateStreamOnHGlobal(Default::default(), true) }.unwrap();
    unsafe {
        let mut written = 0;
        file.CopyTo(&target, u64::MAX, None, Some(&mut written))
            .unwrap();
        assert_eq!(written, 8);
        target.Seek(0, STREAM_SEEK_SET, None).unwrap();
        let mut bytes = [0u8; 8];
        let mut read = 0;
        target
            .Read(bytes.as_mut_ptr().cast(), 8, Some(&mut read))
            .ok()
            .unwrap();
        assert_eq!(read, 8);
        assert_eq!(&bytes, b"SFTP\0OLE");
        asynchronous
            .EndOperation(S_OK, None::<&IBindCtx>, DROPEFFECT_COPY.0)
            .unwrap();
    }
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    notices
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
}

#[test]
fn escape_cancels_the_drag_before_sftp_starts() {
    let (_ole, _runtime, transfer, notices) = setup(
        None,
        Box::new(|_| Box::pin(async { panic!("started a canceled download") })),
    );
    let data = data_object(transfer.clone()).unwrap();
    let file = stream(&data);
    let source: IDropSource = FileDropSource {
        transfer: transfer.clone(),
    }
    .into();
    assert_eq!(
        unsafe { source.QueryContinueDrag(true, MK_LBUTTON) },
        DRAGDROP_S_CANCEL
    );
    transfer.finish(DRAGDROP_S_CANCEL, 0, false);
    let mut buffer = [0u8; 1];
    assert_eq!(
        unsafe { file.Read(buffer.as_mut_ptr().cast(), 1, None) },
        E_PENDING
    );
    assert!(notices.try_recv().is_err());
}

#[test]
fn a_failed_snapshot_returns_a_com_error_and_a_safe_notice() {
    let (_ole, _runtime, transfer, notices) = setup(
        Some(1),
        Box::new(|_| Box::pin(async { anyhow::bail!("private remote path") })),
    );
    let data = data_object(transfer.clone()).unwrap();
    let source: IDropSource = FileDropSource {
        transfer: transfer.clone(),
    }
    .into();
    assert_eq!(
        unsafe { source.QueryContinueDrag(false, MODIFIERKEYS_FLAGS(0)) },
        DRAGDROP_S_DROP
    );
    let file = stream(&data);
    let mut buffer = [0u8; 1];
    assert_eq!(
        unsafe { file.Read(buffer.as_mut_ptr().cast(), 1, None) },
        STG_E_READFAULT
    );
    transfer.finish(DRAGDROP_S_DROP, 0, false);
    let error = notices
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap_err();
    assert_eq!(error.to_string(), "Could not download the remote file");
}
