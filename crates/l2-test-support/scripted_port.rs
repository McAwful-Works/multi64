//! Serial port stand-in for the L2 pipe unit tests.
//!
//! This is not a crate. `multi64-sc64-l2` and `multi64-ed64-l2` each compile it into their own test
//! build only, with
//!
//! ```text
//! #[cfg(test)]
//! #[path = "../../l2-test-support/scripted_port.rs"]
//! mod scripted_port;
//! ```
//!
//! so it is never part of either crate's public API. Use only `std` and `serialport` here: both crates
//! depend on those.

use serialport::{ClearBuffer, SerialPort};
use std::collections::VecDeque;
use std::io;
use std::time::Duration;

/// Each `read` hands out the next scripted chunk, then times out. Writes are accepted and dropped.
pub struct ScriptedPort {
    reads: VecDeque<Vec<u8>>,
}

impl ScriptedPort {
    pub fn new(reads: Vec<Vec<u8>>) -> Self {
        Self {
            reads: reads.into(),
        }
    }
}

impl io::Read for ScriptedPort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.reads.pop_front() {
            Some(chunk) => {
                buf[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
            None => Err(io::Error::new(io::ErrorKind::TimedOut, "script done")),
        }
    }
}

impl io::Write for ScriptedPort {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SerialPort for ScriptedPort {
    fn name(&self) -> Option<String> {
        None
    }
    fn baud_rate(&self) -> serialport::Result<u32> {
        unimplemented!()
    }
    fn data_bits(&self) -> serialport::Result<serialport::DataBits> {
        unimplemented!()
    }
    fn flow_control(&self) -> serialport::Result<serialport::FlowControl> {
        unimplemented!()
    }
    fn parity(&self) -> serialport::Result<serialport::Parity> {
        unimplemented!()
    }
    fn stop_bits(&self) -> serialport::Result<serialport::StopBits> {
        unimplemented!()
    }
    fn timeout(&self) -> Duration {
        Duration::ZERO
    }
    fn set_baud_rate(&mut self, _: u32) -> serialport::Result<()> {
        unimplemented!()
    }
    fn set_data_bits(&mut self, _: serialport::DataBits) -> serialport::Result<()> {
        unimplemented!()
    }
    fn set_flow_control(&mut self, _: serialport::FlowControl) -> serialport::Result<()> {
        unimplemented!()
    }
    fn set_parity(&mut self, _: serialport::Parity) -> serialport::Result<()> {
        unimplemented!()
    }
    fn set_stop_bits(&mut self, _: serialport::StopBits) -> serialport::Result<()> {
        unimplemented!()
    }
    fn set_timeout(&mut self, _: Duration) -> serialport::Result<()> {
        Ok(())
    }
    fn write_request_to_send(&mut self, _: bool) -> serialport::Result<()> {
        unimplemented!()
    }
    fn write_data_terminal_ready(&mut self, _: bool) -> serialport::Result<()> {
        unimplemented!()
    }
    fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
        unimplemented!()
    }
    fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
        unimplemented!()
    }
    fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
        unimplemented!()
    }
    fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
        unimplemented!()
    }
    fn bytes_to_read(&self) -> serialport::Result<u32> {
        unimplemented!()
    }
    fn bytes_to_write(&self) -> serialport::Result<u32> {
        unimplemented!()
    }
    fn clear(&self, _: ClearBuffer) -> serialport::Result<()> {
        Ok(())
    }
    fn try_clone(&self) -> serialport::Result<Box<dyn SerialPort>> {
        unimplemented!()
    }
    fn set_break(&self) -> serialport::Result<()> {
        unimplemented!()
    }
    fn clear_break(&self) -> serialport::Result<()> {
        unimplemented!()
    }
}
