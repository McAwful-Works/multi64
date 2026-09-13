//! The byte pipe [`crate::Ed64Pro`] talks through: a real serial port, or a scripted mock in tests.

use serialport::{ClearBuffer, SerialPort};
use std::io::{self, Read, Write};
use std::time::Duration;

/// What the protocol needs from a serial connection.
pub trait Transport: Read + Write {
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()>;
    /// Discard anything already received (edlink's `FlushPort`).
    fn clear_input(&mut self) -> io::Result<()>;
    /// Bytes waiting to be read without blocking (edlink's `BytesToRead`).
    fn bytes_to_read(&mut self) -> io::Result<u32>;
}

impl Transport for Box<dyn SerialPort> {
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        SerialPort::set_timeout(self.as_mut(), timeout).map_err(io::Error::other)
    }

    fn clear_input(&mut self) -> io::Result<()> {
        self.clear(ClearBuffer::Input).map_err(io::Error::other)
    }

    fn bytes_to_read(&mut self) -> io::Result<u32> {
        SerialPort::bytes_to_read(self.as_ref()).map_err(io::Error::other)
    }
}

/// A transport that records every write and answers reads from a script, for host-only tests.
#[cfg(test)]
pub(crate) mod mock {
    use super::Transport;
    use std::collections::VecDeque;
    use std::io::{self, Read, Write};
    use std::time::Duration;

    #[derive(Default)]
    pub struct Scripted {
        /// Every byte written, in order.
        pub written: Vec<u8>,
        /// Size of each individual `write` call, to check how data was split.
        pub write_sizes: Vec<usize>,
        pub timeouts: Vec<Duration>,
        replies: VecDeque<u8>,
    }

    impl Scripted {
        pub fn replying(bytes: &[u8]) -> Self {
            Self {
                replies: bytes.iter().copied().collect(),
                ..Self::default()
            }
        }

        pub fn unread(&self) -> usize {
            self.replies.len()
        }
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.replies.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "script exhausted: the device would not have replied",
                ));
            }
            let n = buf.len().min(self.replies.len());
            for slot in buf.iter_mut().take(n) {
                *slot = self.replies.pop_front().expect("length checked");
            }
            Ok(n)
        }
    }

    impl Write for Scripted {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            self.write_sizes.push(buf.len());
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Transport for Scripted {
        fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
            self.timeouts.push(timeout);
            Ok(())
        }

        fn clear_input(&mut self) -> io::Result<()> {
            // Leave the script alone: in a test, scripted replies are the device's future answers,
            // not stale bytes that were already sitting in the buffer.
            Ok(())
        }

        fn bytes_to_read(&mut self) -> io::Result<u32> {
            Ok(self.replies.len() as u32)
        }
    }
}
