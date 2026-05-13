//! In-memory block device for tests and [`crate::partition::PartitionDiskUnion::Ram`].
//!
//! Backs a single partition image: byte offsets `0..=len-1` map to the volume.

use std::io::{self, Read, Seek, SeekFrom, Write};

/// Used by exFAT FAT entry helpers so they work on both SC64 and RAM disks.
pub(crate) trait PartitionDisk: Read + Write + Seek {
    fn partition_byte_len(&self) -> u64;
}

use std::sync::{Arc, Mutex};

/// Same role as [`crate::partition::Sc64PartitionDisk`], but over a shared [`Vec<u8>`].
pub(crate) struct RamPartitionDisk {
    storage: Arc<Mutex<Vec<u8>>>,
    pos: u64,
    writable: bool,
}

impl RamPartitionDisk {
    pub(crate) fn new_writable(storage: Arc<Mutex<Vec<u8>>>) -> Self {
        Self {
            storage,
            pos: 0,
            writable: true,
        }
    }

    pub(crate) fn new_readonly(storage: Arc<Mutex<Vec<u8>>>) -> Self {
        Self {
            storage,
            pos: 0,
            writable: false,
        }
    }
}

impl PartitionDisk for RamPartitionDisk {
    fn partition_byte_len(&self) -> u64 {
        self.storage.lock().map(|g| g.len() as u64).unwrap_or(0)
    }
}

impl Read for RamPartitionDisk {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let g = self
            .storage
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let len = g.len() as u64;
        if self.pos >= len {
            return Ok(0);
        }
        let n = buf.len().min((len - self.pos) as usize);
        let start = self.pos as usize;
        buf[..n].copy_from_slice(&g[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for RamPartitionDisk {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "read-only RAM partition",
            ));
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let mut g = self
            .storage
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let end = self
            .pos
            .checked_add(buf.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "write overflow"))?;
        if end > g.len() as u64 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "write past end of RAM partition",
            ));
        }
        let start = self.pos as usize;
        g[start..start + buf.len()].copy_from_slice(buf);
        self.pos += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for RamPartitionDisk {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let len = self
            .storage
            .lock()
            .map(|g| g.len() as u64)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let new_pos: i128 = match pos {
            SeekFrom::Start(s) => s as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => len as i128 + d as i128,
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        let n = new_pos as u64;
        if n > len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek past end of RAM partition",
            ));
        }
        self.pos = n;
        Ok(self.pos)
    }
}
