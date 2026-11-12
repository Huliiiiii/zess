use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    mem::zeroed,
    net::Shutdown,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{null, null_mut},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use windows_sys::Win32::{
    Foundation::{
        ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED,
        ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, SetNamedPipeHandleState, WaitNamedPipeW,
        },
        Threading::{
            CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
        },
    },
};

use crate::{SocketPath, windows::security::PipeSecurity};

const PACKET_BYTES: usize = 64 * 1024;
const DATA: u8 = 0;
const EOF: u8 = 1;

struct Shared {
    handle: Mutex<Option<Arc<OwnedHandle>>>,
    read_shutdown: OwnedHandle,
    write_shutdown: OwnedHandle,
    nonblocking: AtomicBool,
    read_closed: AtomicBool,
    write_closed: AtomicBool,
    read_timeout: Mutex<Option<Duration>>,
    write_timeout: Mutex<Option<Duration>>,
    input: Mutex<VecDeque<u8>>,
    output: Mutex<()>,
}

pub struct Stream(Arc<Shared>);

impl Stream {
    fn new(handle: OwnedHandle) -> io::Result<Self> {
        Ok(Self(Arc::new(Shared {
            handle: Mutex::new(Some(Arc::new(handle))),
            read_shutdown: event()?,
            write_shutdown: event()?,
            nonblocking: AtomicBool::new(false),
            read_closed: AtomicBool::new(false),
            write_closed: AtomicBool::new(false),
            read_timeout: Mutex::new(None),
            write_timeout: Mutex::new(None),
            input: Mutex::new(VecDeque::new()),
            output: Mutex::new(()),
        })))
    }

    pub fn connect(socket: &SocketPath) -> io::Result<Self> {
        let name = socket.native_name().map_err(io::Error::other)?;
        let handle = loop {
            // SAFETY: name is NUL-terminated; returned handle is uniquely owned.
            let raw = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    null_mut(),
                )
            };
            if raw != INVALID_HANDLE_VALUE {
                // SAFETY: successful CreateFileW transfers ownership.
                break unsafe { OwnedHandle::from_raw_handle(raw) };
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
                return Err(error);
            }
            // SAFETY: name remains a valid NUL-terminated pipe name.
            if unsafe { WaitNamedPipeW(name.as_ptr(), 2000) } == 0 {
                return Err(io::Error::last_os_error());
            }
        };
        let mode = PIPE_READMODE_MESSAGE;
        // SAFETY: handle is a duplex pipe and mode points to a valid input integer.
        if unsafe { SetNamedPipeHandleState(handle.as_raw_handle(), &mode, null(), null()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Self::new(handle)
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self(Arc::clone(&self.0)))
    }

    pub(crate) fn handle(&self) -> io::Result<Arc<OwnedHandle>> {
        self.0
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        if matches!(how, Shutdown::Read | Shutdown::Both) {
            self.0.read_closed.store(true, Ordering::Release);
            // SAFETY: the event remains owned by Shared for every active reader.
            unsafe {
                SetEvent(self.0.read_shutdown.as_raw_handle());
            }
        }
        if matches!(how, Shutdown::Both) {
            self.0.write_closed.store(true, Ordering::Release);
            // SAFETY: active operations retain Shared and their own handle Arc;
            // the write event wakes each request so it can cancel and settle.
            unsafe {
                SetEvent(self.0.write_shutdown.as_raw_handle());
            }
            self.0.handle.lock().unwrap().take();
            return Ok(());
        }
        if matches!(how, Shutdown::Write | Shutdown::Both) {
            let _output = self.0.output.lock().unwrap();
            if !self.0.write_closed.load(Ordering::Acquire) {
                let timeout = *self.0.write_timeout.lock().unwrap();
                self.write_packet(&[EOF], timeout, false)?;
                self.0.write_closed.store(true, Ordering::Release);
            }
        }
        Ok(())
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.nonblocking.store(nonblocking, Ordering::Release);
        Ok(())
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        validate_timeout(timeout)?;
        *self.0.read_timeout.lock().unwrap() = timeout;
        Ok(())
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        validate_timeout(timeout)?;
        *self.0.write_timeout.lock().unwrap() = timeout;
        Ok(())
    }

    fn write_packet(
        &self,
        bytes: &[u8],
        timeout: Option<Duration>,
        nonblocking: bool,
    ) -> io::Result<usize> {
        let handle = self.handle()?;
        operation(
            handle.as_raw_handle(),
            timeout,
            nonblocking,
            Some(self.0.write_shutdown.as_raw_handle()),
            |overlapped| {
                // SAFETY: operation settles the request before bytes/overlapped expire.
                unsafe {
                    WriteFile(
                        handle.as_raw_handle(),
                        bytes.as_ptr(),
                        bytes.len() as u32,
                        null_mut(),
                        overlapped,
                    )
                }
            },
        )
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut input = self.0.input.lock().unwrap();
        if input.is_empty() {
            if self.0.read_closed.load(Ordering::Acquire) {
                return Ok(0);
            }
            let timeout = *self.0.read_timeout.lock().unwrap();
            let mut packet = vec![0u8; PACKET_BYTES + 1];
            let handle = match self.handle() {
                Ok(handle) => handle,
                Err(_) if self.0.read_closed.load(Ordering::Acquire) => return Ok(0),
                Err(error) => return Err(error),
            };
            let read = operation(
                handle.as_raw_handle(),
                timeout,
                self.0.nonblocking.load(Ordering::Acquire),
                Some(self.0.read_shutdown.as_raw_handle()),
                |overlapped| {
                    // SAFETY: operation settles before the buffer/overlapped expire.
                    unsafe {
                        ReadFile(
                            handle.as_raw_handle(),
                            packet.as_mut_ptr(),
                            packet.len() as u32,
                            null_mut(),
                            overlapped,
                        )
                    }
                },
            );
            let count = match read {
                Ok(count) => count,
                Err(error) if matches!(error.raw_os_error(), Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_NO_DATA as i32) =>
                {
                    return Ok(0);
                }
                Err(_) if self.0.read_closed.load(Ordering::Acquire) => return Ok(0),
                Err(error) => return Err(error),
            };
            match packet.get(..count) {
                Some([DATA, payload @ ..]) if !payload.is_empty() => input.extend(payload),
                Some([EOF]) => {
                    self.0.read_closed.store(true, Ordering::Release);
                    return Ok(0);
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid local pipe packet",
                    ));
                }
            }
        }
        let count = buffer.len().min(input.len());
        for byte in &mut buffer[..count] {
            *byte = input.pop_front().unwrap();
        }
        Ok(count)
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let _output = self.0.output.lock().unwrap();
        if self.0.write_closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "pipe write side is closed",
            ));
        }
        let count = buffer.len().min(PACKET_BYTES);
        let mut packet = Vec::with_capacity(count + 1);
        packet.push(DATA);
        packet.extend_from_slice(&buffer[..count]);
        let timeout = *self.0.write_timeout.lock().unwrap();
        let written =
            self.write_packet(&packet, timeout, self.0.nonblocking.load(Ordering::Acquire))?;
        if written != packet.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "incomplete local pipe packet",
            ));
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct Listener {
    handle: Mutex<OwnedHandle>,
    socket: SocketPath,
    nonblocking: AtomicBool,
}

impl Listener {
    pub fn bind(socket: &SocketPath) -> Result<Self> {
        let handle = create_instance(socket, true).context("bind local server pipe")?;
        Ok(Self {
            handle: Mutex::new(handle),
            socket: socket.clone(),
            nonblocking: AtomicBool::new(false),
        })
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Ordering::Release);
        Ok(())
    }

    pub fn accept(&self) -> io::Result<Stream> {
        let mut handle = self.handle.lock().unwrap();
        let result = operation(
            handle.as_raw_handle(),
            None,
            self.nonblocking.load(Ordering::Acquire),
            None,
            |overlapped| {
                // SAFETY: operation settles ConnectNamedPipe before overlapped expires.
                unsafe { ConnectNamedPipe(handle.as_raw_handle(), overlapped) }
            },
        );
        match result {
            Ok(_) => {}
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) => {}
            Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA as i32) => {
                *handle = create_instance(&self.socket, false)?;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            Err(error) => return Err(error),
        }
        let next = create_instance(&self.socket, false)?;
        Stream::new(std::mem::replace(&mut *handle, next))
    }
}

fn create_instance(socket: &SocketPath, first: bool) -> io::Result<OwnedHandle> {
    let name = socket.native_name().map_err(io::Error::other)?;
    let security = PipeSecurity::new()?;
    let attributes = security.attributes();
    let flags = PIPE_ACCESS_DUPLEX
        | FILE_FLAG_OVERLAPPED
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    // SAFETY: name/attributes remain live, and each successful call transfers a
    // fresh handle. ACL grants only the current SID; remote clients are rejected.
    let raw = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            flags,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            (PACKET_BYTES * 2) as u32,
            (PACKET_BYTES * 2) as u32,
            0,
            &attributes,
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful CreateNamedPipeW transfers unique ownership.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}

fn validate_timeout(timeout: Option<Duration>) -> io::Result<()> {
    if timeout.is_some_and(|duration| duration.is_zero()) {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "timeout must be nonzero",
        ))
    } else {
        Ok(())
    }
}

fn event() -> io::Result<OwnedHandle> {
    // SAFETY: no security attributes/name are supplied; successful event is owned.
    let raw = unsafe { CreateEventW(null(), 1, 0, null()) };
    if raw.is_null() {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: CreateEventW transferred unique ownership of this event.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}

fn operation(
    handle: HANDLE,
    timeout: Option<Duration>,
    nonblocking: bool,
    shutdown: Option<HANDLE>,
    start: impl FnOnce(*mut OVERLAPPED) -> i32,
) -> io::Result<usize> {
    let event = event()?;
    // SAFETY: zero is valid for the OVERLAPPED offsets and reserved fields.
    let mut overlapped: OVERLAPPED = unsafe { zeroed() };
    overlapped.hEvent = event.as_raw_handle();
    let started = start(&mut overlapped);
    let error = if started == 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    if let Some(error) = error
        && error.raw_os_error() != Some(ERROR_IO_PENDING as i32)
    {
        return Err(error);
    }
    let wait = if nonblocking {
        0
    } else {
        timeout
            .map(|duration| duration.as_millis().max(1).min((INFINITE - 1) as u128) as u32)
            .unwrap_or(INFINITE)
    };
    let waited = if let Some(shutdown) = shutdown {
        let handles = [event.as_raw_handle(), shutdown];
        // SAFETY: both handles remain live through this operation; the array
        // contains exactly two initialized event handles.
        unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, wait) }
    } else {
        // SAFETY: event is live and belongs to this request.
        unsafe { WaitForSingleObject(event.as_raw_handle(), wait) }
    };
    let cancelled = waited != WAIT_OBJECT_0;
    let shutting_down = shutdown.is_some() && waited == WAIT_OBJECT_0 + 1;
    let wait_error = if waited != WAIT_OBJECT_0 && waited != WAIT_TIMEOUT && !shutting_down {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    if cancelled {
        // SAFETY: request is still owned here; only this operation is cancelled.
        unsafe {
            CancelIoEx(handle, &raw const overlapped);
        }
    }
    let mut count = 0;
    // SAFETY: wait=true settles completion even after cancellation. No buffer or
    // OVERLAPPED memory is released while the kernel may still access it.
    if unsafe { GetOverlappedResult(handle, &raw const overlapped, &mut count, 1) } != 0 {
        Ok(count as usize)
    } else {
        let error = io::Error::last_os_error();
        if let Some(error) = wait_error {
            return Err(error);
        }
        if cancelled && error.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) {
            Err(io::Error::from(if shutting_down {
                io::ErrorKind::Interrupted
            } else if nonblocking {
                io::ErrorKind::WouldBlock
            } else {
                io::ErrorKind::TimedOut
            }))
        } else {
            Err(error)
        }
    }
}
