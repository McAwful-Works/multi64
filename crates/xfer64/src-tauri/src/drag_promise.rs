//! Windows drag-and-drop **file promises** for the SD card pane.
//!
//! A cart file is not on disk, and Windows only copies bytes that exist at drop time — which is
//! why handing the shell a path means exporting the file first and dragging twice. A promise
//! inverts that: the drag starts immediately carrying a *description* of the files
//! (`CFSTR_FILEDESCRIPTORW`), and the shell asks for each one's bytes (`CFSTR_FILECONTENTS`) only
//! once it knows where they are going. We answer with an `IStream` that reads off the cart over
//! serial as the shell drains it, and Explorer shows its own copy progress.
//!
//! Everything above [`mod win`] is plain Rust and testable anywhere: the descriptor is a byte
//! layout, and the stream is a pipe. The COM glue is Windows-only and has to be exercised on a
//! Windows box — `cargo check --target x86_64-pc-windows-msvc` is the most a Linux CI can do.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Condvar, Mutex};

/// One file offered to the shell.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromisedFile {
    /// Path on the cart, e.g. `roms/sm64.z64`.
    pub cart_path: String,
    /// Name the file gets at the destination.
    pub name: String,
    /// Size in bytes, from the pane's listing — the shell wants it up front, for its progress
    /// bar and its free-space check, long before it asks for any bytes.
    pub size: u64,
}

// ---------------------------------------------------------------------------
// FILEGROUPDESCRIPTORW
//
// Written out by hand rather than through the `windows` structs so the layout is one thing, on
// every platform, with tests. `FILEDESCRIPTORW` is 592 bytes: flags(4) clsid(16) sizel(8)
// pointl(8) attributes(4) three FILETIMEs(24) size high/low(8) name(520).
// ---------------------------------------------------------------------------

/// `sizeof(FILEDESCRIPTORW)`.
const FILE_DESCRIPTOR_BYTES: usize = 592;
/// Offset of `cFileName` within a descriptor.
const FILE_DESCRIPTOR_NAME_OFFSET: usize = 72;
/// `cFileName` is `[u16; 260]`, NUL included.
const FILE_DESCRIPTOR_NAME_CHARS: usize = 260;

/// `FD_ATTRIBUTES | FD_FILESIZE | FD_PROGRESSUI` — which fields below we actually filled in, plus
/// a request for the shell's own progress dialog while it drains our streams.
const FD_FLAGS: u32 = 0x0004 | 0x0040 | 0x4000;
/// `FILE_ATTRIBUTE_NORMAL`.
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

/// Serialise `files` as a `FILEGROUPDESCRIPTORW`: a `UINT` count, then one descriptor each.
pub fn file_group_descriptor_bytes(files: &[PromisedFile]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + files.len() * FILE_DESCRIPTOR_BYTES);
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for f in files {
        let start = out.len();
        out.resize(start + FILE_DESCRIPTOR_BYTES, 0);
        let d = &mut out[start..start + FILE_DESCRIPTOR_BYTES];
        d[0..4].copy_from_slice(&FD_FLAGS.to_le_bytes());
        d[36..40].copy_from_slice(&FILE_ATTRIBUTE_NORMAL.to_le_bytes());
        d[64..68].copy_from_slice(&((f.size >> 32) as u32).to_le_bytes());
        d[68..72].copy_from_slice(&((f.size & 0xffff_ffff) as u32).to_le_bytes());
        // UTF-16, NUL-terminated, truncated to fit. Truncation keeps whole code units: a name cut
        // mid-surrogate reaches the shell as a replacement glyph in the copied file's name.
        let mut units: Vec<u16> = f.name.encode_utf16().collect();
        units.truncate(FILE_DESCRIPTOR_NAME_CHARS - 1);
        if units.last().is_some_and(|u| (0xd800..0xdc00).contains(u)) {
            units.pop();
        }
        for (i, u) in units.iter().enumerate() {
            let at = FILE_DESCRIPTOR_NAME_OFFSET + i * 2;
            d[at..at + 2].copy_from_slice(&u.to_le_bytes());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The pipe behind each promised stream
// ---------------------------------------------------------------------------

/// Bytes buffered ahead of the shell. The cart reads in large chunks and the shell drains in
/// whatever size it likes, so this only has to absorb the difference — not the file.
const PIPE_CAPACITY_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
struct PipeState {
    buf: VecDeque<u8>,
    /// The reader is gone: the drag was cancelled, or the shell stopped early.
    closed: bool,
    /// The writer finished; `err` says whether it finished well.
    done: bool,
    err: Option<String>,
}

type PipeShared = Arc<(Mutex<PipeState>, Condvar)>;

fn new_pipe() -> (PipeWriter, PipeReader) {
    let shared: PipeShared = Arc::new((Mutex::new(PipeState::default()), Condvar::new()));
    (
        PipeWriter {
            shared: Arc::clone(&shared),
        },
        PipeReader { shared },
    )
}

/// Write half, handed to the cart reader thread.
pub struct PipeWriter {
    shared: PipeShared,
}

/// A handle for asking "has the reader gone?" without borrowing the writer.
///
/// The cart read holds the writer for its whole run, so the cancel check needs its own handle:
/// the progress callback carries one of these and aborts a drag that was cancelled halfway
/// through a 64 MB ROM, instead of reading it to the end for nobody.
#[derive(Clone)]
pub struct PipeClosedProbe {
    shared: PipeShared,
}

impl PipeClosedProbe {
    pub fn is_closed(&self) -> bool {
        self.shared.0.lock().map(|s| s.closed).unwrap_or(true)
    }
}

impl PipeWriter {
    /// A probe that outlives the borrow of this writer.
    pub fn closed_probe(&self) -> PipeClosedProbe {
        PipeClosedProbe {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Record how the read ended and wake the reader.
    pub fn finish(self, result: io::Result<()>) {
        if let Ok(mut s) = self.shared.0.lock() {
            s.done = true;
            if let Err(e) = result {
                // A cancelled drag is how this normally ends; it is not a failure to report.
                if e.kind() != io::ErrorKind::Interrupted && !s.closed {
                    s.err = Some(e.to_string());
                }
            }
        }
        self.shared.1.notify_all();
    }
}

impl io::Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (lock, cvar) = &*self.shared;
        let mut state = lock.lock().map_err(|_| io::Error::other("pipe poisoned"))?;
        while state.buf.len() >= PIPE_CAPACITY_BYTES && !state.closed {
            state = cvar
                .wait(state)
                .map_err(|_| io::Error::other("pipe poisoned"))?;
        }
        if state.closed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "drag cancelled"));
        }
        let room = PIPE_CAPACITY_BYTES - state.buf.len();
        let n = buf.len().min(room);
        state.buf.extend(&buf[..n]);
        drop(state);
        cvar.notify_all();
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Read half, drained by the shell through `IStream::Read`.
pub struct PipeReader {
    shared: PipeShared,
}

impl io::Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let (lock, cvar) = &*self.shared;
        let mut state = lock.lock().map_err(|_| io::Error::other("pipe poisoned"))?;
        while state.buf.is_empty() && !state.done {
            state = cvar
                .wait(state)
                .map_err(|_| io::Error::other("pipe poisoned"))?;
        }
        if state.buf.is_empty() {
            if let Some(e) = state.err.clone() {
                return Err(io::Error::other(e));
            }
            return Ok(0);
        }
        let n = buf.len().min(state.buf.len());
        for (i, b) in state.buf.drain(..n).enumerate() {
            buf[i] = b;
        }
        drop(state);
        cvar.notify_all();
        Ok(n)
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        if let Ok(mut s) = self.shared.0.lock() {
            s.closed = true;
        }
        self.shared.1.notify_all();
    }
}

/// Opens a byte stream for one cart file. Called when the shell asks for the contents — which is
/// after the drop, so this is where the serial traffic starts, not at drag start.
pub trait CartFileSource: Send + Sync + 'static {
    fn open(&self, cart_path: &str) -> io::Result<PipeReader>;
}

/// Start the cart reader for `cart_path` on its own thread and hand back the read half.
///
/// `read` is the blocking cart read; it gets the writer and a "should I stop?" probe, so a
/// cancelled drag stops the serial traffic instead of finishing the file for nobody.
pub fn spawn_cart_reader<F>(read: F) -> PipeReader
where
    F: FnOnce(&mut PipeWriter) -> io::Result<()> + Send + 'static,
{
    let (mut writer, reader) = new_pipe();
    std::thread::spawn(move || {
        let result = read(&mut writer);
        writer.finish(result);
    });
    reader
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn promised(name: &str, size: u64) -> PromisedFile {
        PromisedFile {
            cart_path: format!("roms/{name}"),
            name: name.to_string(),
            size,
        }
    }

    /// The shell reads the count, then fixed-size records; a byte out of place here is a drag
    /// that copies nothing, or copies with a mangled name.
    #[test]
    fn descriptor_layout_matches_filegroupdescriptorw() {
        let files = vec![promised("sm64.z64", 8 * 1024 * 1024), promised("a.bin", 1)];
        let bytes = file_group_descriptor_bytes(&files);

        assert_eq!(bytes.len(), 4 + 2 * FILE_DESCRIPTOR_BYTES);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 2);

        let first = &bytes[4..4 + FILE_DESCRIPTOR_BYTES];
        assert_eq!(
            u32::from_le_bytes(first[0..4].try_into().unwrap()),
            FD_FLAGS
        );
        assert_eq!(
            u32::from_le_bytes(first[36..40].try_into().unwrap()),
            FILE_ATTRIBUTE_NORMAL
        );
        assert_eq!(u32::from_le_bytes(first[64..68].try_into().unwrap()), 0);
        assert_eq!(
            u32::from_le_bytes(first[68..72].try_into().unwrap()),
            8 * 1024 * 1024
        );
        let name: Vec<u16> = first[FILE_DESCRIPTOR_NAME_OFFSET..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|u| *u != 0)
            .collect();
        assert_eq!(String::from_utf16(&name).unwrap(), "sm64.z64");
    }

    /// A size over 4 GiB has to split across the high and low words, or the shell truncates the
    /// copy at the low 32 bits.
    #[test]
    fn descriptor_splits_large_sizes_across_both_words() {
        let bytes = file_group_descriptor_bytes(&[promised("big.bin", 0x1_0000_0005)]);
        let d = &bytes[4..];
        assert_eq!(u32::from_le_bytes(d[64..68].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(d[68..72].try_into().unwrap()), 5);
    }

    /// The name field is fixed-size and must stay NUL-terminated, whatever the cart calls a file.
    #[test]
    fn descriptor_truncates_long_names_and_keeps_the_terminator() {
        let long = "x".repeat(400);
        let bytes = file_group_descriptor_bytes(&[promised(&long, 1)]);
        let d = &bytes[4..];
        let units: Vec<u16> = d[FILE_DESCRIPTOR_NAME_OFFSET..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(units.len(), FILE_DESCRIPTOR_NAME_CHARS);
        assert_eq!(units[FILE_DESCRIPTOR_NAME_CHARS - 1], 0);
        assert_eq!(units.iter().take_while(|u| **u != 0).count(), 259);
    }

    /// Truncation must not cut a surrogate pair in half.
    #[test]
    fn descriptor_truncation_keeps_surrogate_pairs_whole() {
        // 259 units of filler + an emoji would land the pair's high half on the boundary.
        let name = format!("{}{}", "y".repeat(258), '\u{1F600}');
        let bytes = file_group_descriptor_bytes(&[promised(&name, 1)]);
        let d = &bytes[4..];
        let units: Vec<u16> = d[FILE_DESCRIPTOR_NAME_OFFSET..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|u| *u != 0)
            .collect();
        assert!(String::from_utf16(&units).is_ok(), "name must stay valid");
    }

    /// The shell drains in its own chunk sizes while the cart fills in ours.
    #[test]
    fn pipe_carries_every_byte_across_mismatched_chunk_sizes() {
        let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
        let expected = payload.clone();
        let mut reader = spawn_cart_reader(move |w| {
            for chunk in payload.chunks(7919) {
                w.write_all(chunk)?;
            }
            Ok(())
        });

        let mut got = Vec::new();
        let mut buf = [0u8; 1031];
        loop {
            let n = reader.read(&mut buf).expect("read");
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, expected);
    }

    /// Dropping the reader is a cancelled drag: the cart read must stop, not run to the end.
    ///
    /// Two things can notice — the probe between writes, and a write already blocked on a full
    /// pipe — and which one wins is a race, so this asserts only that the read *ends*.
    #[test]
    fn dropping_the_reader_stops_the_cart_read() {
        let (exited, saw_exit) = std::sync::mpsc::channel();
        let mut reader = spawn_cart_reader(move |w| {
            let cancelled = w.closed_probe();
            let big = vec![0u8; 64 * 1024];
            let result = loop {
                if cancelled.is_closed() {
                    break Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                }
                if let Err(e) = w.write_all(&big) {
                    break Err(e);
                }
            };
            let _ = exited.send(());
            result
        });

        let mut buf = [0u8; 4096];
        let first = reader.read(&mut buf).expect("first read");
        assert!(first > 0, "the reader should see bytes before cancelling");
        drop(reader);

        assert!(
            saw_exit
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok(),
            "cart read did not stop after the drag was cancelled"
        );
    }

    /// A read that fails partway has to surface, not look like a short file.
    #[test]
    fn reader_surfaces_a_failed_cart_read() {
        let mut reader = spawn_cart_reader(|w| {
            w.write_all(b"partial")?;
            Err(io::Error::other("serial went away"))
        });
        let mut got = Vec::new();
        let err = loop {
            let mut buf = [0u8; 64];
            match reader.read(&mut buf) {
                Ok(0) => panic!("expected an error, got clean EOF"),
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) => break e,
            }
        };
        assert_eq!(got, b"partial");
        assert!(err.to_string().contains("serial went away"));
    }
}

// ---------------------------------------------------------------------------
// The COM side. Windows only: none of these interfaces exist elsewhere, and the point of a
// promise is the shell's copy engine on the other end of them.
//
// Written against windows-rs 0.61, where `#[implement]` generates a `Foo_Impl` type and the
// interface traits are implemented for *that* — `self` reaches the fields by Deref.
// ---------------------------------------------------------------------------
#[cfg(windows)]
pub mod win {
    use super::{file_group_descriptor_bytes, CartFileSource, PipeReader, PromisedFile};
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex, Once};
    use windows::Win32::Foundation::{
        DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC,
        DV_E_TYMED, E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, HGLOBAL, OLE_E_ADVISENOTSUPPORTED,
        STG_E_ACCESSDENIED, STG_E_INVALIDFUNCTION, S_FALSE, S_OK,
    };
    use windows::Win32::System::Com::{
        IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA,
        ISequentialStream_Impl, IStream, IStream_Impl, DVASPECT_CONTENT, FORMATETC, LOCKTYPE,
        STATFLAG, STATSTG, STGC, STGMEDIUM, STGMEDIUM_0, STREAM_SEEK, STREAM_SEEK_CUR,
        STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTREAM,
    };
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::{
        DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, DROPEFFECT, DROPEFFECT_COPY,
    };
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;
    use windows_core::{
        implement, Error as WinError, Ref, Result as WinResult, BOOL, HRESULT, PCWSTR,
    };

    /// `OleInitialize` is per-thread and must not run twice on the thread we drag from.
    static OLE_INIT: Once = Once::new();

    fn init_ole() {
        OLE_INIT.call_once(|| unsafe {
            let _ = OleInitialize(None);
        });
    }

    fn clipboard_format(name: &str) -> u16 {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        (unsafe { RegisterClipboardFormatW(PCWSTR::from_raw(wide.as_ptr())) }) as u16
    }

    /// The three clipboard formats a file promise is made of.
    struct Formats {
        descriptor: u16,
        contents: u16,
        preferred_effect: u16,
    }

    impl Formats {
        fn register() -> Self {
            Self {
                descriptor: clipboard_format("FileGroupDescriptorW"),
                contents: clipboard_format("FileContents"),
                preferred_effect: clipboard_format("Preferred DropEffect"),
            }
        }
    }

    fn hglobal_from_bytes(bytes: &[u8]) -> WinResult<HGLOBAL> {
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
            let ptr = GlobalLock(handle);
            if ptr.is_null() {
                return Err(WinError::from(E_OUTOFMEMORY));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
            let _ = GlobalUnlock(handle);
            Ok(handle)
        }
    }

    fn medium_from_hglobal(handle: HGLOBAL) -> STGMEDIUM {
        STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: handle },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        }
    }

    /// One promised file's bytes, pulled off the cart as the shell reads them.
    #[implement(IStream)]
    struct PromiseStream {
        reader: Mutex<PipeReader>,
        size: u64,
        position: Mutex<u64>,
    }

    impl PromiseStream {
        fn new(reader: PipeReader, size: u64) -> Self {
            Self {
                reader: Mutex::new(reader),
                size,
                position: Mutex::new(0),
            }
        }
    }

    #[allow(non_snake_case)]
    impl ISequentialStream_Impl for PromiseStream_Impl {
        fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
            if pv.is_null() {
                return E_POINTER;
            }
            let out = unsafe { std::slice::from_raw_parts_mut(pv as *mut u8, cb as usize) };
            let Ok(mut guard) = self.reader.lock() else {
                return STG_E_INVALIDFUNCTION;
            };
            // One call, one pipe read: a short read is legal and the shell asks again. Blocking
            // here is fine — the cart fills the pipe from its own thread.
            match std::io::Read::read(&mut *guard, out) {
                Ok(n) => {
                    if let Ok(mut p) = self.position.lock() {
                        *p += n as u64;
                    }
                    if !pcbread.is_null() {
                        unsafe { *pcbread = n as u32 };
                    }
                    if n == 0 {
                        S_FALSE
                    } else {
                        S_OK
                    }
                }
                Err(_) => {
                    if !pcbread.is_null() {
                        unsafe { *pcbread = 0 };
                    }
                    STG_E_INVALIDFUNCTION
                }
            }
        }

        fn Write(&self, _pv: *const c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
            STG_E_ACCESSDENIED
        }
    }

    #[allow(non_snake_case)]
    impl IStream_Impl for PromiseStream_Impl {
        fn Seek(
            &self,
            dlibmove: i64,
            dworigin: STREAM_SEEK,
            plibnewposition: *mut u64,
        ) -> WinResult<()> {
            let position = *self
                .position
                .lock()
                .map_err(|_| WinError::from(STG_E_INVALIDFUNCTION))?;
            // The cart is read forwards, once. "Where am I", and a seek to where we already are,
            // are the only ones that can be honoured.
            let here = (dworigin == STREAM_SEEK_CUR && dlibmove == 0)
                || (dworigin == STREAM_SEEK_SET && dlibmove >= 0 && dlibmove as u64 == position);
            if !here {
                return Err(WinError::from(E_NOTIMPL));
            }
            if !plibnewposition.is_null() {
                unsafe { *plibnewposition = position };
            }
            Ok(())
        }

        fn SetSize(&self, _libnewsize: u64) -> WinResult<()> {
            Err(WinError::from(STG_E_ACCESSDENIED))
        }

        fn CopyTo(
            &self,
            pstm: Ref<'_, IStream>,
            cb: u64,
            pcbread: *mut u64,
            pcbwritten: *mut u64,
        ) -> WinResult<()> {
            let dest = pstm.ok()?;
            let mut buf = vec![0u8; 256 * 1024];
            let mut moved = 0u64;
            while moved < cb {
                let want = buf.len().min((cb - moved) as usize);
                let mut got = 0u32;
                let hr = ISequentialStream_Impl::Read(
                    self,
                    buf.as_mut_ptr() as *mut c_void,
                    want as u32,
                    &mut got,
                );
                if hr.is_err() {
                    return Err(WinError::from(hr));
                }
                if got == 0 {
                    break;
                }
                unsafe { dest.Write(buf.as_ptr() as *const c_void, got, None) }.ok()?;
                moved += got as u64;
            }
            if !pcbread.is_null() {
                unsafe { *pcbread = moved };
            }
            if !pcbwritten.is_null() {
                unsafe { *pcbwritten = moved };
            }
            Ok(())
        }

        fn Commit(&self, _grfcommitflags: &STGC) -> WinResult<()> {
            Ok(())
        }

        fn Revert(&self) -> WinResult<()> {
            Ok(())
        }

        fn LockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: &LOCKTYPE) -> WinResult<()> {
            Err(WinError::from(STG_E_INVALIDFUNCTION))
        }

        fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> WinResult<()> {
            Err(WinError::from(STG_E_INVALIDFUNCTION))
        }

        fn Stat(&self, pstatstg: *mut STATSTG, _grfstatflag: &STATFLAG) -> WinResult<()> {
            if pstatstg.is_null() {
                return Err(WinError::from(E_POINTER));
            }
            // The shell's progress bar and its free-space check both come from here, before it
            // asks for a single byte.
            let stat = STATSTG {
                cbSize: self.size,
                r#type: 2, // STGTY_STREAM
                ..Default::default()
            };
            unsafe { *pstatstg = stat };
            Ok(())
        }

        fn Clone(&self) -> WinResult<IStream> {
            // A clone would need a second reader over the one serial port.
            Err(WinError::from(E_NOTIMPL))
        }
    }

    /// The promise: a description of the files now, their bytes when the shell asks.
    #[implement(IDataObject)]
    struct PromiseDataObject {
        files: Vec<PromisedFile>,
        source: Arc<dyn CartFileSource>,
        formats: Formats,
    }

    #[allow(non_snake_case)]
    impl IDataObject_Impl for PromiseDataObject_Impl {
        fn GetData(&self, pformatetcin: *const FORMATETC) -> WinResult<STGMEDIUM> {
            let format =
                unsafe { pformatetcin.as_ref() }.ok_or_else(|| WinError::from(E_POINTER))?;
            let cf = format.cfFormat;

            if cf == self.formats.descriptor {
                let bytes = file_group_descriptor_bytes(&self.files);
                return Ok(medium_from_hglobal(hglobal_from_bytes(&bytes)?));
            }

            if cf == self.formats.preferred_effect {
                return Ok(medium_from_hglobal(hglobal_from_bytes(
                    &DROPEFFECT_COPY.0.to_le_bytes(),
                )?));
            }

            if cf == self.formats.contents {
                // lindex picks the file, and this call is where serial traffic begins — after the
                // drop, once the shell knows where the bytes are going.
                let file = usize::try_from(format.lindex)
                    .ok()
                    .and_then(|i| self.files.get(i))
                    .ok_or_else(|| WinError::from(DV_E_FORMATETC))?;
                let reader = self
                    .source
                    .open(&file.cart_path)
                    .map_err(|_| WinError::from(STG_E_INVALIDFUNCTION))?;
                let stream: IStream = PromiseStream::new(reader, file.size).into();
                return Ok(STGMEDIUM {
                    tymed: TYMED_ISTREAM.0 as u32,
                    u: STGMEDIUM_0 {
                        pstm: std::mem::ManuallyDrop::new(Some(stream)),
                    },
                    pUnkForRelease: std::mem::ManuallyDrop::new(None),
                });
            }

            Err(WinError::from(DV_E_FORMATETC))
        }

        fn GetDataHere(
            &self,
            _pformatetc: *const FORMATETC,
            _pmedium: *mut STGMEDIUM,
        ) -> WinResult<()> {
            Err(WinError::from(E_NOTIMPL))
        }

        fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
            let Some(format) = (unsafe { pformatetc.as_ref() }) else {
                return E_POINTER;
            };
            let cf = format.cfFormat;
            let known = cf == self.formats.descriptor
                || cf == self.formats.contents
                || cf == self.formats.preferred_effect;
            if !known {
                return DV_E_FORMATETC;
            }
            let wanted = if cf == self.formats.contents {
                TYMED_ISTREAM.0 as u32
            } else {
                TYMED_HGLOBAL.0 as u32
            };
            if format.tymed & wanted == 0 {
                return DV_E_TYMED;
            }
            S_OK
        }

        fn GetCanonicalFormatEtc(
            &self,
            _pformatectin: *const FORMATETC,
            pformatetcout: *mut FORMATETC,
        ) -> HRESULT {
            if !pformatetcout.is_null() {
                unsafe { (*pformatetcout).ptd = std::ptr::null_mut() };
            }
            // Nothing to canonicalise: every format we offer is already in its only shape.
            S_FALSE
        }

        fn SetData(
            &self,
            _pformatetc: *const FORMATETC,
            _pmedium: *const STGMEDIUM,
            _frelease: BOOL,
        ) -> WinResult<()> {
            Err(WinError::from(E_NOTIMPL))
        }

        fn EnumFormatEtc(&self, dwdirection: u32) -> WinResult<IEnumFORMATETC> {
            // DATADIR_GET only; we never accept data.
            if dwdirection != 1 {
                return Err(WinError::from(E_NOTIMPL));
            }
            let descriptor = FORMATETC {
                cfFormat: self.formats.descriptor,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            let effect = FORMATETC {
                cfFormat: self.formats.preferred_effect,
                ..descriptor
            };
            let contents = FORMATETC {
                cfFormat: self.formats.contents,
                tymed: TYMED_ISTREAM.0 as u32,
                ..descriptor
            };
            unsafe { SHCreateStdEnumFmtEtc(&[descriptor, contents, effect]) }
        }

        fn DAdvise(
            &self,
            _pformatetc: *const FORMATETC,
            _advf: u32,
            _padvsink: Ref<'_, IAdviseSink>,
        ) -> WinResult<u32> {
            Err(WinError::from(OLE_E_ADVISENOTSUPPORTED))
        }

        fn DUnadvise(&self, _dwconnection: u32) -> WinResult<()> {
            Err(WinError::from(OLE_E_ADVISENOTSUPPORTED))
        }

        fn EnumDAdvise(&self) -> WinResult<IEnumSTATDATA> {
            Err(WinError::from(OLE_E_ADVISENOTSUPPORTED))
        }
    }

    /// Button still down keeps the drag alive, Escape cancels, release drops.
    #[implement(IDropSource)]
    struct PromiseDropSource;

    #[allow(non_snake_case)]
    impl IDropSource_Impl for PromiseDropSource_Impl {
        fn QueryContinueDrag(
            &self,
            fescapepressed: BOOL,
            grfkeystate: MODIFIERKEYS_FLAGS,
        ) -> HRESULT {
            if fescapepressed.as_bool() {
                DRAGDROP_S_CANCEL
            } else if (grfkeystate & MK_LBUTTON) == MODIFIERKEYS_FLAGS(0) {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        }

        fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    /// Run the drag, returning whether it ended in a drop.
    ///
    /// Blocks: `DoDragDrop` is modal, and with a synchronous data object the shell's copy happens
    /// inside it, calling back into our streams. Must run on the thread owning the message loop.
    pub fn run_promise_drag(
        files: Vec<PromisedFile>,
        source: Arc<dyn CartFileSource>,
    ) -> Result<bool, String> {
        init_ole();
        let data: IDataObject = PromiseDataObject {
            files,
            source,
            formats: Formats::register(),
        }
        .into();
        let drop_source: IDropSource = PromiseDropSource.into();
        let mut effect = DROPEFFECT::default();
        let result = unsafe { DoDragDrop(&data, &drop_source, DROPEFFECT_COPY, &mut effect) };
        if result == DRAGDROP_S_DROP {
            Ok(true)
        } else if result == DRAGDROP_S_CANCEL {
            Ok(false)
        } else {
            Err(format!("DoDragDrop failed: 0x{:08x}", result.0))
        }
    }
}
