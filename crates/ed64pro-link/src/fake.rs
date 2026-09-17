//! An in-memory EverDrive-64 PRO for host-only tests.
//!
//! It decodes the bytes a cart would receive — using this crate's reading of the protocol — and
//! answers from an in-memory file system and memory map. That checks host code against
//! `docs/spec/ed64-pro-usb-host.md` end to end, and **nothing about hardware**: wherever the spec is
//! wrong, this fake is wrong in exactly the same way.
//!
//! Status codes are invented (see [`status`]); neither Krikzz source documents the firmware's.

use crate::transport::Transport;
use crate::wire::FIFO_ADDR;
use crate::wire::{
    fs, open_mode, Endpoint, ACK_BLOCK, ATTR_DIR, CMD_EPO, CMD_FS, CMD_NRESP, CMD_STATUS,
    CMD_STATUS2, DEVICE_ID_ED64_PRO, EPO_SCMD_XFER, PROTOCOL_ID, STATUS_KEY,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::time::Duration;

/// Status codes this fake reports. The real firmware's values are unknown.
pub mod status {
    pub const OK: u8 = 0x00;
    pub const NOT_FOUND: u8 = 0x04;
    pub const NOT_EMPTY: u8 = 0x07;
    pub const EXISTS: u8 = 0x08;
    pub const NO_FILE_OPEN: u8 = 0x09;
    pub const SHORT_READ: u8 = 0x0A;
    pub const BAD_COMMAND: u8 = 0x0F;
}

const ATTR_ARCHIVE: u8 = 0x20;
const FAKE_DATE: u16 = 0x5A21;
const FAKE_TIME: u16 = 0x6000;

/// A file (`Some(bytes)`) or directory (`None`), keyed by its lowercased path.
struct Node {
    /// Path with the case it was created with.
    display: String,
    data: Option<Vec<u8>>,
}

struct OpenFile {
    key: String,
    pos: usize,
}

/// A multi-part exchange still waiting for host bytes.
enum Pending {
    Command,
    AckedWrite {
        remaining: usize,
        block: usize,
        buf: Vec<u8>,
    },
    RawMemory {
        addr: u32,
        remaining: usize,
    },
}

pub struct FakeEd64Pro {
    inbox: Vec<u8>,
    outbox: VecDeque<u8>,
    last_status: u8,
    nodes: BTreeMap<String, Node>,
    open: Option<OpenFile>,
    listing: Vec<String>,
    pending: Pending,
    memory: HashMap<u32, u8>,
    /// Bytes queued for the ROM at [`FIFO_ADDR`], in arrival order.
    fifo: Vec<u8>,
    /// Directory loads so far, and the 1-based number of the one to refuse (0 for none).
    dir_loads: u32,
    fail_dir_load: u32,
}

impl Default for FakeEd64Pro {
    fn default() -> Self {
        Self::new()
    }
}

fn norm(path: &str) -> String {
    path.trim().replace('\\', "/").trim_matches('/').to_string()
}

fn key(path: &str) -> String {
    norm(path).to_lowercase()
}

fn parent_key(key: &str) -> &str {
    key.rsplit_once('/').map(|(p, _)| p).unwrap_or("")
}

fn string_arg(a: &[u8]) -> String {
    let n = u16::from_be_bytes([a[0], a[1]]) as usize;
    String::from_utf8_lossy(&a[2..2 + n]).into_owned()
}

impl FakeEd64Pro {
    /// An empty SD card.
    pub fn new() -> Self {
        Self {
            inbox: Vec::new(),
            outbox: VecDeque::new(),
            last_status: status::OK,
            nodes: BTreeMap::new(),
            open: None,
            listing: Vec::new(),
            pending: Pending::Command,
            memory: HashMap::new(),
            fifo: Vec::new(),
            dir_loads: 0,
            fail_dir_load: 0,
        }
    }

    /// Refuse the `n`th directory load (1-based, counting from now), as a cart that could not list a
    /// folder would.
    pub fn failing_dir_load(mut self, n: u32) -> Self {
        self.fail_dir_load = self.dir_loads + n;
        self
    }

    /// Add a directory, creating parents.
    pub fn with_dir(mut self, path: &str) -> Self {
        self.insert_dir_all(path);
        self
    }

    /// Add a file, creating parent directories.
    pub fn with_file(mut self, path: &str, data: &[u8]) -> Self {
        let p = norm(path);
        if let Some((parent, _)) = p.rsplit_once('/') {
            self.insert_dir_all(parent);
        }
        self.nodes.insert(
            p.to_lowercase(),
            Node {
                display: p,
                data: Some(data.to_vec()),
            },
        );
        self
    }

    /// Contents of a file, or `None` if absent or a directory.
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.nodes.get(&key(path)).and_then(|n| n.data.as_deref())
    }

    pub fn is_dir(&self, path: &str) -> bool {
        self.is_dir_key(&key(path))
    }

    pub fn exists(&self, path: &str) -> bool {
        let k = key(path);
        k.is_empty() || self.nodes.contains_key(&k)
    }

    /// Bytes of cart memory; unwritten addresses read as zero.
    pub fn memory(&self, addr: u32, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| *self.memory.get(&addr.wrapping_add(i as u32)).unwrap_or(&0))
            .collect()
    }

    /// Everything the host has queued for a running ROM with `fifo_write`, in order.
    ///
    /// The cart's FIFO is a queue, not memory: consecutive writes append rather than overwrite,
    /// and the ROM drains it (ed64-pro-pub `ed_fifo_rda`). Nothing here drains it.
    pub fn fifo_received(&self) -> &[u8] {
        &self.fifo
    }

    /// Queue bytes as if a running ROM had sent them with `ed_usb_wr`.
    pub fn push_usb(&mut self, bytes: &[u8]) {
        self.outbox.extend(bytes.iter().copied());
    }

    fn insert_dir_all(&mut self, path: &str) {
        let mut acc = String::new();
        for part in norm(path).split('/').filter(|s| !s.is_empty()) {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            self.nodes.entry(acc.to_lowercase()).or_insert(Node {
                display: acc.clone(),
                data: None,
            });
        }
    }

    fn is_dir_key(&self, key: &str) -> bool {
        key.is_empty() || matches!(self.nodes.get(key), Some(Node { data: None, .. }))
    }

    /// Direct children of a directory: directories first, then by name.
    fn children(&self, dir_key: &str) -> Vec<String> {
        let mut out: Vec<&String> = self
            .nodes
            .keys()
            .filter(|k| parent_key(k) == dir_key)
            .collect();
        out.sort_by(|a, b| {
            let a_dir = self.nodes[*a].data.is_none();
            let b_dir = self.nodes[*b].data.is_none();
            b_dir.cmp(&a_dir).then_with(|| a.cmp(b))
        });
        out.into_iter().cloned().collect()
    }

    /// A file record: size, date, time, attributes, name (spec §7).
    fn record(&self, key: &str) -> Option<Vec<u8>> {
        let node = self.nodes.get(key)?;
        let name = node.display.rsplit('/').next().unwrap_or(&node.display);
        let (size, attr) = match &node.data {
            Some(d) => (d.len() as u32, ATTR_ARCHIVE),
            None => (0, ATTR_DIR),
        };
        let mut r = Vec::with_capacity(11 + name.len());
        r.extend_from_slice(&size.to_be_bytes());
        r.extend_from_slice(&FAKE_DATE.to_be_bytes());
        r.extend_from_slice(&FAKE_TIME.to_be_bytes());
        r.push(attr);
        r.extend_from_slice(&(name.len() as u16).to_be_bytes());
        r.extend_from_slice(name.as_bytes());
        Some(r)
    }

    fn process(&mut self) {
        loop {
            match std::mem::replace(&mut self.pending, Pending::Command) {
                Pending::AckedWrite {
                    mut remaining,
                    mut block,
                    mut buf,
                } => {
                    if self.inbox.len() < block {
                        self.pending = Pending::AckedWrite {
                            remaining,
                            block,
                            buf,
                        };
                        return;
                    }
                    buf.extend(self.inbox.drain(..block));
                    remaining -= block;
                    if remaining > 0 {
                        block = remaining.min(ACK_BLOCK);
                        self.outbox.push_back(0);
                        self.pending = Pending::AckedWrite {
                            remaining,
                            block,
                            buf,
                        };
                    } else {
                        self.finish_file_write(&buf);
                    }
                }
                Pending::RawMemory { addr, remaining } => {
                    let n = remaining.min(self.inbox.len());
                    let bytes: Vec<u8> = self.inbox.drain(..n).collect();
                    let next = if addr == FIFO_ADDR {
                        // A queue: the address does not advance.
                        self.fifo.extend_from_slice(&bytes);
                        addr
                    } else {
                        for (i, b) in bytes.into_iter().enumerate() {
                            self.memory.insert(addr.wrapping_add(i as u32), b);
                        }
                        addr.wrapping_add(n as u32)
                    };
                    if n < remaining {
                        self.pending = Pending::RawMemory {
                            addr: next,
                            remaining: remaining - n,
                        };
                        return;
                    }
                }
                Pending::Command => {
                    if !self.step_command() {
                        return;
                    }
                }
            }
        }
    }

    /// Decode and run one command. `false` when more bytes are needed.
    fn step_command(&mut self) -> bool {
        // Skip to the next `2B D4`: this passes over the host's wake-up zeros and any noise.
        match self.inbox.windows(2).position(|w| w == [b'+', b'+' ^ 0xFF]) {
            Some(0) => {}
            Some(n) => {
                self.inbox.drain(..n);
            }
            None => {
                let keep = usize::from(self.inbox.last() == Some(&b'+'));
                let drop_n = self.inbox.len() - keep;
                self.inbox.drain(..drop_n);
                return false;
            }
        }
        if self.inbox.len() < 4 {
            return false;
        }
        let cmd = self.inbox[2];
        if self.inbox[3] != cmd ^ 0xFF {
            self.inbox.drain(..1);
            return true;
        }
        let need = match args_len(cmd, &self.inbox[4..]) {
            Some(n) => n,
            None => return false,
        };
        let frame: Vec<u8> = self.inbox.drain(..4 + need).collect();
        self.execute(cmd, &frame[4..]);
        true
    }

    fn execute(&mut self, cmd: u8, a: &[u8]) {
        match cmd {
            CMD_STATUS => self.outbox.extend([
                STATUS_KEY,
                PROTOCOL_ID,
                DEVICE_ID_ED64_PRO,
                self.last_status,
            ]),
            // A Gen3 cart does not answer STATUS2 (spec §4).
            CMD_STATUS2 => {}
            CMD_NRESP => self.outbox.push_back(0x01),
            CMD_FS => self.fs_command(a[0], &a[1..]),
            CMD_EPO if a[0] == EPO_SCMD_XFER => self.epo(&a[1..18]),
            _ => self.last_status = status::BAD_COMMAND,
        }
    }

    fn fs_command(&mut self, scmd: u8, a: &[u8]) {
        self.last_status = match scmd {
            fs::INIT => status::OK,
            fs::FILE_OPEN => {
                let path = string_arg(&a[1..]);
                self.open_file(&path, a[0])
            }
            fs::FILE_CLOSE => {
                if self.open.take().is_some() {
                    status::OK
                } else {
                    status::NO_FILE_OPEN
                }
            }
            fs::AVAILABLE => {
                let left = match &self.open {
                    Some(o) => self
                        .nodes
                        .get(&o.key)
                        .and_then(|n| n.data.as_ref())
                        .map(|d| d.len().saturating_sub(o.pos))
                        .unwrap_or(0) as u64,
                    None => 0,
                };
                self.outbox.extend((left as u32).to_be_bytes());
                self.outbox.extend(((left >> 32) as u32).to_be_bytes());
                return;
            }
            fs::FILE_SEEK => match self.open.as_mut() {
                Some(o) => {
                    o.pos = u32::from_be_bytes([a[0], a[1], a[2], a[3]]) as usize;
                    status::OK
                }
                None => status::NO_FILE_OPEN,
            },
            fs::FILE_INFO => {
                match self.record(&key(&string_arg(a))) {
                    Some(r) => {
                        self.outbox.push_back(0);
                        self.outbox.extend(r);
                    }
                    None => self.outbox.push_back(status::NOT_FOUND),
                }
                return;
            }
            fs::DIR_LOAD => {
                let k = key(&string_arg(&a[1..]));
                self.dir_loads += 1;
                if self.dir_loads == self.fail_dir_load {
                    status::BAD_COMMAND
                } else if self.is_dir_key(&k) {
                    self.listing = self.children(&k);
                    status::OK
                } else {
                    status::NOT_FOUND
                }
            }
            fs::DIR_SIZE => {
                self.outbox
                    .extend((self.listing.len() as u16).to_be_bytes());
                return;
            }
            fs::DIR_GET => {
                let start = u16::from_be_bytes([a[0], a[1]]) as usize;
                let count = u16::from_be_bytes([a[2], a[3]]) as usize;
                for i in start..start + count {
                    match self.listing.get(i).and_then(|k| self.record(k)) {
                        Some(r) => {
                            self.outbox.push_back(0);
                            self.outbox.extend(r);
                        }
                        None => self.outbox.push_back(status::NOT_FOUND),
                    }
                }
                return;
            }
            fs::DIR_MAKE => {
                let path = string_arg(a);
                let k = key(&path);
                if k.is_empty() || self.nodes.contains_key(&k) {
                    status::EXISTS
                } else if !self.is_dir_key(parent_key(&k)) {
                    status::NOT_FOUND
                } else {
                    self.nodes.insert(
                        k,
                        Node {
                            display: norm(&path),
                            data: None,
                        },
                    );
                    status::OK
                }
            }
            fs::DELETE => {
                let k = key(&string_arg(a));
                match self.nodes.get(&k) {
                    None => status::NOT_FOUND,
                    Some(n) if n.data.is_none() && !self.children(&k).is_empty() => {
                        status::NOT_EMPTY
                    }
                    Some(_) => {
                        self.nodes.remove(&k);
                        if self.open.as_ref().is_some_and(|o| o.key == k) {
                            self.open = None;
                        }
                        status::OK
                    }
                }
            }
            fs::DIR_TEST => {
                if self.is_dir_key(&key(&string_arg(a))) {
                    status::OK
                } else {
                    status::NOT_FOUND
                }
            }
            fs::FILE_TEST => {
                if self.file(&string_arg(a)).is_some() {
                    status::OK
                } else {
                    status::NOT_FOUND
                }
            }
            _ => status::BAD_COMMAND,
        };
    }

    fn open_file(&mut self, path: &str, mode: u8) -> u8 {
        let k = key(path);
        if self.is_dir_key(&k) {
            return status::NOT_FOUND;
        }
        let exists = self.nodes.contains_key(&k);
        if mode & open_mode::WRITE == 0 {
            if !exists {
                return status::NOT_FOUND;
            }
        } else {
            if !self.is_dir_key(parent_key(&k)) {
                if mode & open_mode::MAKE_PATH == 0 {
                    return status::NOT_FOUND;
                }
                if let Some((parent, _)) = norm(path).rsplit_once('/') {
                    self.insert_dir_all(parent);
                }
            }
            if exists && mode & open_mode::CREATE_NEW != 0 {
                return status::EXISTS;
            }
            let create = mode & (open_mode::CREATE_ALWAYS | open_mode::CREATE_NEW) != 0
                || (!exists && mode & open_mode::OPEN_ALWAYS != 0);
            if !exists && !create {
                return status::NOT_FOUND;
            }
            if create {
                // FatFs truncates an existing file in place, so the name keeps the letter case it
                // was stored with, whatever case `path` is in.
                let display = self
                    .nodes
                    .get(&k)
                    .map_or_else(|| norm(path), |n| n.display.clone());
                self.nodes.insert(
                    k.clone(),
                    Node {
                        display,
                        data: Some(Vec::new()),
                    },
                );
            }
        }
        self.open = Some(OpenFile { key: k, pos: 0 });
        status::OK
    }

    fn finish_file_write(&mut self, data: &[u8]) {
        self.last_status = match self.open.as_mut() {
            None => status::NO_FILE_OPEN,
            Some(o) => match self.nodes.get_mut(&o.key).and_then(|n| n.data.as_mut()) {
                None => status::NOT_FOUND,
                Some(file) => {
                    let end = o.pos + data.len();
                    if file.len() < end {
                        file.resize(end, 0);
                    }
                    file[o.pos..end].copy_from_slice(data);
                    o.pos = end;
                    status::OK
                }
            },
        };
    }

    fn epo(&mut self, x: &[u8]) {
        let src_addr = u32::from_be_bytes([x[0], x[1], x[2], x[3]]);
        let dst_addr = u32::from_be_bytes([x[4], x[5], x[6], x[7]]);
        let len = u32::from_be_bytes([x[8], x[9], x[10], x[11]]) as usize;
        let (src, dst) = (x[12], x[13]);

        if src == Endpoint::File as u8 && dst == Endpoint::Link as u8 {
            // Always send `len` bytes: the host reads exactly that many before asking for status.
            let (out, st) = match self.open.as_mut() {
                None => (vec![0; len], status::NO_FILE_OPEN),
                Some(o) => {
                    let data = self
                        .nodes
                        .get(&o.key)
                        .and_then(|n| n.data.as_deref())
                        .unwrap_or(&[]);
                    let from = o.pos.min(data.len());
                    let to = (o.pos + len).min(data.len());
                    let mut out = data[from..to].to_vec();
                    let st = if out.len() < len {
                        status::SHORT_READ
                    } else {
                        status::OK
                    };
                    out.resize(len, 0);
                    o.pos += len;
                    (out, st)
                }
            };
            self.outbox.extend(out);
            self.last_status = st;
        } else if src == Endpoint::LinkAck as u8 && dst == Endpoint::File as u8 {
            if len == 0 {
                self.last_status = status::OK;
                return;
            }
            self.outbox.push_back(0);
            self.pending = Pending::AckedWrite {
                remaining: len,
                block: len.min(ACK_BLOCK),
                buf: Vec::with_capacity(len),
            };
        } else if src == Endpoint::Link as u8 && dst == Endpoint::Memory as u8 {
            if len > 0 {
                self.pending = Pending::RawMemory {
                    addr: dst_addr,
                    remaining: len,
                };
            }
        } else if src == Endpoint::Memory as u8 && dst == Endpoint::Link as u8 {
            let out = self.memory(src_addr, len);
            self.outbox.extend(out);
            self.last_status = status::OK;
        } else {
            self.last_status = status::BAD_COMMAND;
        }
    }
}

/// Argument bytes after a command frame, or `None` if they have not all arrived.
fn args_len(cmd: u8, a: &[u8]) -> Option<usize> {
    let string_at = |off: usize| -> Option<usize> {
        if a.len() < off + 2 {
            return None;
        }
        let n = u16::from_be_bytes([a[off], a[off + 1]]) as usize;
        (a.len() >= off + 2 + n).then_some(off + 2 + n)
    };
    match cmd {
        CMD_STATUS | CMD_STATUS2 => Some(0),
        CMD_NRESP => (!a.is_empty()).then_some(1),
        CMD_FS => match *a.first()? {
            fs::INIT | fs::FILE_CLOSE | fs::AVAILABLE | fs::DIR_SIZE => Some(1),
            fs::FILE_SEEK => (a.len() >= 5).then_some(5),
            fs::FILE_OPEN | fs::DIR_LOAD => string_at(2),
            fs::FILE_INFO | fs::DIR_MAKE | fs::DELETE | fs::DIR_TEST | fs::FILE_TEST => {
                string_at(1)
            }
            fs::DIR_GET => (a.len() >= 7).then_some(7),
            _ => Some(1),
        },
        CMD_EPO => (a.len() >= 18).then_some(18),
        _ => Some(0),
    }
}

impl Read for FakeEd64Pro {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.outbox.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "fake EverDrive-64 PRO has nothing to send",
            ));
        }
        let n = buf.len().min(self.outbox.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.outbox.pop_front().expect("length checked");
        }
        Ok(n)
    }
}

impl Write for FakeEd64Pro {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inbox.extend_from_slice(buf);
        self.process();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Transport for FakeEd64Pro {
    fn set_timeout(&mut self, _timeout: Duration) -> io::Result<()> {
        Ok(())
    }

    fn clear_input(&mut self) -> io::Result<()> {
        self.outbox.clear();
        Ok(())
    }

    fn bytes_to_read(&mut self) -> io::Result<u32> {
        Ok(self.outbox.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{dir_option, Ed64Pro, Error};

    #[test]
    fn host_round_trips_files_directories_and_memory_through_the_fake() {
        let fake = FakeEd64Pro::new().with_file("ed64/menu.cfg", b"theme=dark");
        let mut dev = Ed64Pro::connect(fake).expect("handshake");
        dev.fs_init().unwrap();

        dev.dir_make("roms").unwrap();
        let data: Vec<u8> = (0..3000u32).map(|i| (i * 7) as u8).collect();
        dev.file_open("roms/game.z64", open_mode::WRITE | open_mode::CREATE_ALWAYS)
            .unwrap();
        dev.file_write(&data).unwrap();
        dev.file_close().unwrap();

        let root = dev.dir_list("", dir_option::SORTED).unwrap();
        let names: Vec<_> = root.iter().map(|e| (e.name.as_str(), e.is_dir())).collect();
        assert_eq!(names, [("ed64", true), ("roms", true)]);

        dev.file_open("roms/game.z64", open_mode::READ).unwrap();
        assert_eq!(dev.file_available().unwrap(), 3000);
        let mut back = vec![0u8; 3000];
        dev.file_read(&mut back).unwrap();
        dev.file_close().unwrap();
        assert_eq!(back, data);

        assert!(dev.file_exists("roms/game.z64").unwrap());
        assert!(dev.dir_exists("roms").unwrap());
        let info = dev.file_info("ed64/menu.cfg").unwrap();
        assert_eq!((info.size, info.is_dir()), (10, false));

        dev.fifo_write(b"hello ").unwrap();
        dev.fifo_write(b"rom").unwrap();
        dev.mem_write(0x1000_0000, b"ram").unwrap();
        let mut ram = [0u8; 3];
        dev.mem_read(0x1000_0000, &mut ram).unwrap();
        assert_eq!(&ram, b"ram");

        let err = dev.delete("roms").unwrap_err();
        assert!(
            matches!(err, Error::Operation { status: 0x07, .. }),
            "{err}"
        );
        dev.delete("roms/game.z64").unwrap();
        dev.delete("roms").unwrap();
        assert!(!dev.dir_exists("roms").unwrap());

        let mut fake = dev.into_inner();
        assert!(fake.file("ed64/menu.cfg").is_some());
        assert_eq!(
            fake.fifo_received(),
            b"hello rom",
            "FIFO writes queue in order"
        );
        fake.push_usb(b"log");
        let mut dev = Ed64Pro::connect(fake).expect("reconnect");
        let mut buf = [0u8; 8];
        // The reconnect's clear_input discards pending ROM output, as edlink's FlushPort does.
        assert_eq!(dev.usb_read(&mut buf).unwrap(), 0);
    }

    /// FatFs `f_open` with `FA_CREATE_ALWAYS` on a name that matches an existing file only after
    /// case folding truncates that file under its stored name; it does not rename it.
    #[test]
    fn overwriting_under_another_letter_case_keeps_the_stored_name() {
        let fake = FakeEd64Pro::new().with_file("roms/Game.z64", b"old contents");
        let mut dev = Ed64Pro::connect(fake).expect("handshake");
        dev.fs_init().unwrap();

        dev.file_open("ROMS/GAME.Z64", open_mode::WRITE | open_mode::CREATE_ALWAYS)
            .unwrap();
        dev.file_write(b"new").unwrap();
        dev.file_close().unwrap();

        let names: Vec<_> = dev
            .dir_list("roms", dir_option::SORTED)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["Game.z64"]);
        assert_eq!(dev.file_info("roms/game.z64").unwrap().size, 3);
        assert_eq!(dev.into_inner().file("roms/Game.z64").unwrap(), b"new");
    }
}
