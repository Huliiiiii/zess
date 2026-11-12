use std::{
    io,
    mem::{size_of, zeroed},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::null_mut,
};

use anyhow::{Context, Result};
use windows_sys::Win32::{
    Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_SEM_TIMEOUT, INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    Storage::FileSystem::{FindClose, FindFirstFileW, FindNextFileW, WIN32_FIND_DATAW},
    System::{
        Pipes::WaitNamedPipeW,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

use crate::SocketPath;

pub(crate) fn current_user_sid() -> io::Result<String> {
    // SAFETY: GetCurrentProcess returns a permanently valid pseudo-handle.
    process_user_sid(unsafe { GetCurrentProcess() })
}

pub(crate) fn process_user_sid(
    process: windows_sys::Win32::Foundation::HANDLE,
) -> io::Result<String> {
    // SAFETY: the output pointer is valid and the process pseudo-handle is live.
    let token = unsafe {
        let mut token = null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        OwnedHandle::from_raw_handle(token)
    };
    let mut needed = 0;
    // SAFETY: a zero-length first call queries the required allocation size.
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // usize storage provides the alignment required by TOKEN_USER and its SID.
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    // SAFETY: buffer has the queried size and alignment; token remains owned.
    unsafe {
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut length = 0;
        while *sid.add(length) != 0 {
            length += 1;
        }
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(sid, length));
        LocalFree(sid.cast());
        Ok(value)
    }
}

pub(crate) struct PipeSecurity(PSECURITY_DESCRIPTOR);

impl PipeSecurity {
    pub(crate) fn new() -> io::Result<Self> {
        let descriptor = format!("D:P(A;;GA;;;{})", current_user_sid()?)
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut security = null_mut();
        // SAFETY: descriptor is NUL-terminated; successful output is LocalAlloc-owned.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                descriptor.as_ptr(),
                1,
                &mut security,
                null_mut(),
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(security))
        }
    }

    pub(crate) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: this value uniquely owns the descriptor returned by conversion.
        unsafe {
            LocalFree(self.0);
        }
    }
}

pub(crate) fn discover() -> Result<Vec<SocketPath>> {
    let prefix = format!("zess-{}-", current_user_sid()?);
    let pattern = r"\\.\pipe\*"
        .to_owned()
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: WIN32_FIND_DATAW consists of integers and fixed buffers, all zero-valid.
    let mut data: WIN32_FIND_DATAW = unsafe { zeroed() };
    // SAFETY: pattern and output buffer remain valid throughout the call.
    let search = unsafe { FindFirstFileW(pattern.as_ptr(), &mut data) };
    if search == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
            Ok(Vec::new())
        } else {
            Err(error).context("enumerate server pipes")
        };
    }
    struct Search(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Search {
        fn drop(&mut self) {
            // SAFETY: this uniquely owns a successful FindFirstFileW search handle.
            unsafe {
                FindClose(self.0);
            }
        }
    }
    let _search = Search(search);
    let mut endpoints = Vec::new();
    loop {
        let length = data
            .cFileName
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(data.cFileName.len());
        let name = String::from_utf16_lossy(&data.cFileName[..length]);
        if let Some(id) = name.strip_prefix(&prefix).and_then(|id| id.parse().ok()) {
            endpoints.push(SocketPath::by_id(id));
        }
        // SAFETY: search is live and data is writable.
        if unsafe { FindNextFileW(search, &mut data) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error()
                != Some(windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES as i32)
            {
                return Err(error).context("enumerate server pipes");
            }
            break;
        }
    }
    endpoints.sort();
    endpoints.dedup();
    Ok(endpoints)
}

pub(crate) fn is_present(socket: &SocketPath) -> io::Result<bool> {
    let name = socket.native_name().map_err(io::Error::other)?;
    // SAFETY: name is NUL-terminated. Waiting does not consume a connection.
    if unsafe { WaitNamedPipeW(name.as_ptr(), 1) } != 0 {
        Ok(true)
    } else {
        let error = io::Error::last_os_error();
        match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_FILE_NOT_FOUND) => Ok(false),
            Some(ERROR_PIPE_BUSY | ERROR_SEM_TIMEOUT) => Ok(true),
            _ => Err(error),
        }
    }
}
