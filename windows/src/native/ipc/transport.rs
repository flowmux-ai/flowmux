// SPDX-License-Identifier: GPL-3.0-or-later
//! One outstanding overlapped operation per pipe, with deadlines and cancellation.
use super::*;
use std::{io, time::Instant};
use windows_sys::Win32::System::IO::*;

#[derive(Debug)]
pub(super) struct Event(OwnedHandle);
impl Event {
    pub fn new() -> io::Result<Self> {
        let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(raw) }))
    }
    pub fn signal(&self) {
        unsafe {
            SetEvent(self.0.as_raw_handle());
        }
    }
    pub fn is_set(&self) -> bool {
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }
}

#[derive(Debug)]
pub(super) struct Pipe {
    handle: OwnedHandle,
    ready: Event,
}
enum Operation<'a> {
    Connect,
    Read(&'a mut [u8]),
    Write(&'a [u8]),
}
impl Pipe {
    pub fn client_process(&self) -> io::Result<OwnedHandle> {
        let mut pid = 0;
        if unsafe { GetNamedPipeClientProcessId(self.handle.as_raw_handle(), &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let raw = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
    pub fn new(handle: OwnedHandle) -> io::Result<Self> {
        Ok(Self {
            handle,
            ready: Event::new()?,
        })
    }
    pub fn disconnect(&self) -> io::Result<()> {
        if unsafe { DisconnectNamedPipe(self.handle.as_raw_handle()) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED as i32) {
            Ok(())
        } else {
            Err(error)
        }
    }
    pub fn connect_once(&mut self, stop: &Event) -> io::Result<()> {
        self.perform(Operation::Connect, stop, None).map(|_| ())
    }
    pub fn accept(&mut self, stop: &Event) -> io::Result<()> {
        loop {
            match self.connect_once(stop) {
                Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA as i32) => {
                    self.disconnect()?
                }
                result => return result,
            }
        }
    }
    pub fn read_frame(
        &mut self,
        limit: usize,
        deadline: Instant,
        stop: &Event,
    ) -> io::Result<Vec<u8>> {
        let mut frame = Vec::new();
        let mut chunk = [0u8; 8192];
        while frame.len() < limit {
            let capacity = chunk.len().min(limit - frame.len());
            let count = self.perform(
                Operation::Read(&mut chunk[..capacity]),
                stop,
                Some(deadline),
            )?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete IPC frame",
                ));
            }
            if let Some(end) = chunk[..count].iter().position(|byte| *byte == b'\n') {
                frame.extend_from_slice(&chunk[..=end]);
                return Ok(frame);
            }
            frame.extend_from_slice(&chunk[..count]);
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame exceeds byte limit",
        ))
    }
    pub fn write_all(
        &mut self,
        mut bytes: &[u8],
        deadline: Instant,
        stop: &Event,
    ) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = self.perform(
                Operation::Write(&bytes[..bytes.len().min(65536)]),
                stop,
                Some(deadline),
            )?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "IPC write made no progress",
                ));
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }
    // Closing a byte-mode pipe can discard unread output. Wait for the peer to
    // close after reading its one reply, with a deadline instead of blocking flush.
    pub fn wait_for_close(&mut self, deadline: Instant, stop: &Event) {
        let mut discard = [0u8; 1024];
        while matches!(self.perform(Operation::Read(&mut discard), stop, Some(deadline)), Ok(n) if n != 0)
        {
        }
    }
    fn perform(
        &mut self,
        operation: Operation<'_>,
        stop: &Event,
        deadline: Option<Instant>,
    ) -> io::Result<usize> {
        if stop.is_set() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "IPC server is stopping",
            ));
        }
        if deadline.is_some_and(|end| Instant::now() >= end) {
            return Err(timed_out());
        }
        if unsafe { ResetEvent(self.ready.0.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut overlapped = OVERLAPPED {
            hEvent: self.ready.0.as_raw_handle(),
            ..Default::default()
        };
        let mut count = 0;
        let connecting = matches!(operation, Operation::Connect);
        let raw = self.handle.as_raw_handle();
        let success = unsafe {
            match operation {
                Operation::Connect => ConnectNamedPipe(raw, &mut overlapped),
                Operation::Read(bytes) => ReadFile(
                    raw,
                    bytes.as_mut_ptr(),
                    bytes.len() as u32,
                    &mut count,
                    &mut overlapped,
                ),
                Operation::Write(bytes) => WriteFile(
                    raw,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut count,
                    &mut overlapped,
                ),
            }
        };
        if success != 0 {
            return Ok(count as usize);
        }
        let error = io::Error::last_os_error();
        if connecting && error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
            return Ok(0);
        }
        if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
            return Err(error);
        }
        let handles = [stop.0.as_raw_handle(), self.ready.0.as_raw_handle()];
        let milliseconds = deadline
            .map(|end| {
                end.saturating_duration_since(Instant::now())
                    .as_millis()
                    .min((INFINITE - 1) as u128) as u32
            })
            .unwrap_or(INFINITE);
        let result = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, milliseconds) };
        if result == WAIT_OBJECT_0 + 1 {
            // A signalled operation event means completion. Waiting here also
            // ensures buffers cannot be released while an operation is pending.
            if unsafe { GetOverlappedResult(raw, &overlapped, &mut count, 1) } != 0 {
                return Ok(count as usize);
            }
            return Err(io::Error::last_os_error());
        }
        let error = if result == WAIT_TIMEOUT {
            timed_out()
        } else if result == WAIT_OBJECT_0 {
            io::Error::new(io::ErrorKind::Interrupted, "IPC server is stopping")
        } else {
            io::Error::last_os_error()
        };
        // CancelIoEx only requests cancellation. Keep both OVERLAPPED and the
        // borrowed buffer alive until the kernel reports completion, even if
        // cancellation raced with success (ERROR_NOT_FOUND).
        unsafe {
            CancelIoEx(raw, &overlapped);
            GetOverlappedResult(raw, &overlapped, &mut count, 1);
        }
        Err(error)
    }
}
fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "IPC I/O deadline exceeded")
}
