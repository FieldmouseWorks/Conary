// apps/conaryd/src/daemon/client/body_stream.rs

//! Unix response-body reads with an optional whole-operation deadline.
//!
//! Checking before every buffer fill also bounds framing metadata that a
//! decoder may assemble through multiple socket reads before yielding data.

use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

pub(super) struct BodyStream {
    reader: BufReader<UnixStream>,
    deadline: Option<Instant>,
}

impl BodyStream {
    pub(super) fn new(reader: BufReader<UnixStream>) -> Self {
        Self {
            reader,
            deadline: None,
        }
    }

    pub(super) fn limit_read_time(&mut self, timeout: Duration) -> io::Result<()> {
        self.deadline = Some(Instant::now().checked_add(timeout).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid daemon body read timeout",
            )
        })?);
        Ok(())
    }
}

impl BufRead for BodyStream {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if let Some(deadline) = self.deadline {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "daemon response body read timed out",
                    )
                })?;
            self.reader.get_ref().set_read_timeout(Some(remaining))?;
        }
        self.reader.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.reader.consume(amount);
    }
}

impl Read for BodyStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let available = self.fill_buf()?;
        let count = available.len().min(out.len());
        out[..count].copy_from_slice(&available[..count]);
        self.consume(count);
        Ok(count)
    }
}
