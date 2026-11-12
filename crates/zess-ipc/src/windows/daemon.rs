use std::{
    io,
    mem::{align_of, size_of, transmute, zeroed},
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::PathBuf,
    process::Command,
    ptr::null_mut,
};

use anyhow::{Context, Result, bail};
use windows_sys::Win32::{
    Foundation::{
        FILETIME, HANDLE, LocalFree, STATUS_BUFFER_OVERFLOW, STATUS_BUFFER_TOO_SMALL,
        STATUS_INFO_LENGTH_MISMATCH, UNICODE_STRING, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    System::{
        JobObjects::{
            IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, QueryInformationJobObject,
        },
        LibraryLoader::{GetModuleHandleW, GetProcAddress},
        Pipes::GetNamedPipeServerProcessId,
        SystemInformation::GetSystemTimeAsFileTime,
        Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
            GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
            WaitForSingleObject,
        },
    },
    UI::Shell::CommandLineToArgvW,
};

use crate::{SocketPath, Stream, windows::security};

pub fn detach(command: &mut Command) -> Result<()> {
    let mut flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    let mut in_job = 0;
    // SAFETY: the current-process pseudo-handle is valid; in_job is writable.
    if unsafe { IsProcessInJob(GetCurrentProcess(), null_mut(), &mut in_job) } == 0 {
        return Err(io::Error::last_os_error()).context("inspect launcher Job Object membership");
    }
    if in_job != 0 {
        // SAFETY: all fields are integers or pointers with valid zero values.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        // SAFETY: a null job selects the launcher's immediate Job Object; limits
        // is writable and the supplied size matches its declared structure.
        if unsafe {
            QueryInformationJobObject(
                null_mut(),
                JobObjectExtendedLimitInformation,
                (&raw mut limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error()).context("inspect launcher Job Object limits");
        }
        let limits = limits.BasicLimitInformation.LimitFlags;
        if limits & JOB_OBJECT_LIMIT_BREAKAWAY_OK != 0 {
            // Win32-OpenSSH uses a kill-on-close job that explicitly permits
            // breakaway; console detachment alone leaves the daemon in that job.
            flags |= CREATE_BREAKAWAY_FROM_JOB;
        } else if limits & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0
            && limits & JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK == 0
        {
            bail!(
                "cannot start a persistent server: the host Job Object kills child processes on close and does not allow breakaway"
            );
        }
    }
    command.creation_flags(flags);
    Ok(())
}

pub fn stop(socket: &SocketPath) -> Result<()> {
    // SAFETY: FILETIME is a pair of zero-valid integer fields.
    let mut connected_before: FILETIME = unsafe { zeroed() };
    // SAFETY: output points to an initialized writable FILETIME.
    unsafe {
        GetSystemTimeAsFileTime(&mut connected_before);
    }
    let stream = Stream::connect(socket).context("connect to server for process identification")?;
    let pipe = stream.handle()?;
    let mut pid = 0;
    // SAFETY: stream owns a connected pipe handle and pid is writable.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error()).context("identify pipe server process");
    }
    if pid == 0 || pid == std::process::id() {
        bail!("refusing to terminate an invalid server process");
    }
    // SAFETY: pid came from the connected pipe. The result is validated before
    // becoming an OwnedHandle, and is retained through termination and waiting.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        return Err(io::Error::last_os_error()).context("open pipe server process");
    }
    // SAFETY: successful OpenProcess transferred unique handle ownership.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: FILETIME contains only integer fields.
    let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) =
        unsafe { zeroed() };
    // SAFETY: process is live and each output points to distinct writable storage.
    if unsafe {
        GetProcessTimes(
            process.as_raw_handle(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return Err(io::Error::last_os_error()).context("read server process identity");
    }
    let filetime =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    if filetime(created) > filetime(connected_before) {
        bail!("pipe server exited before identification; refusing to signal a reused PID");
    }
    if security::process_user_sid(process.as_raw_handle())? != security::current_user_sid()? {
        bail!("refusing to stop a server owned by another user");
    }
    let mut image = vec![0u16; 32768];
    let mut length = image.len() as u32;
    // SAFETY: image owns the declared writable UTF-16 capacity.
    if unsafe {
        QueryFullProcessImageNameW(process.as_raw_handle(), 0, image.as_mut_ptr(), &mut length)
    } == 0
    {
        return Err(io::Error::last_os_error()).context("read pipe server executable");
    }
    let image = PathBuf::from(std::ffi::OsString::from_wide(&image[..length as usize]));
    let name = image
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !name.eq_ignore_ascii_case("zess.exe") || !has_server_argument(process.as_raw_handle())? {
        bail!("refusing to terminate pipe peer {pid}: it is not a Zess server");
    }
    let mut verified_pid = 0;
    // SAFETY: the connection remains live and output is writable.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut verified_pid) } == 0
        || verified_pid != pid
    {
        bail!("pipe server identity changed during identification");
    }
    // SAFETY: process is the owned, validated process handle; handle ownership
    // prevents PID reuse from redirecting this termination to another process.
    if unsafe { TerminateProcess(process.as_raw_handle(), 1) } == 0 {
        return Err(io::Error::last_os_error()).context("terminate server process");
    }
    // SAFETY: process handle has SYNCHRONIZE access and remains live.
    match unsafe { WaitForSingleObject(process.as_raw_handle(), 2000) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => bail!("server {pid} did not exit after termination"),
        _ => Err(io::Error::last_os_error()).context("wait for server exit"),
    }
}

fn has_server_argument(process: HANDLE) -> Result<bool> {
    type QueryProcess =
        unsafe extern "system" fn(HANDLE, i32, *mut std::ffi::c_void, u32, *mut u32) -> i32;
    const PROCESS_COMMAND_LINE_INFORMATION: i32 = 60;
    let library = "ntdll.dll"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: ntdll is loaded in every Windows process; the string is terminated.
    let library = unsafe { GetModuleHandleW(library.as_ptr()) };
    if library.is_null() {
        return Err(io::Error::last_os_error()).context("find process information API");
    }
    // SAFETY: the module remains loaded for this process's lifetime and the name
    // is terminated. Resolve at runtime because this NT API can be unavailable.
    let query = unsafe { GetProcAddress(library, c"NtQueryInformationProcess".as_ptr().cast()) }
        .context("process command-line information API is unavailable")?;
    // SAFETY: this is NtQueryInformationProcess's documented system ABI/signature.
    let query = unsafe { transmute::<unsafe extern "system" fn() -> isize, QueryProcess>(query) };
    let mut needed = 0;
    // SAFETY: the live process handle and zero-size query only write needed.
    let status = unsafe {
        query(
            process,
            PROCESS_COMMAND_LINE_INFORMATION,
            null_mut(),
            0,
            &mut needed,
        )
    };
    if !matches!(
        status,
        STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_TOO_SMALL | STATUS_BUFFER_OVERFLOW
    ) || (needed as usize) < size_of::<UNICODE_STRING>()
    {
        bail!("cannot size pipe server command line (NTSTATUS {status:#x})");
    }
    // usize storage aligns both UNICODE_STRING and the following UTF-16 data.
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    let capacity = needed;
    // SAFETY: the buffer owns at least capacity bytes with the required alignment.
    let status = unsafe {
        query(
            process,
            PROCESS_COMMAND_LINE_INFORMATION,
            buffer.as_mut_ptr().cast(),
            capacity,
            &mut needed,
        )
    };
    if status < 0 || needed > capacity || (needed as usize) < size_of::<UNICODE_STRING>() {
        bail!("cannot read pipe server command line (NTSTATUS {status:#x})");
    }
    // SAFETY: successful output has at least the size and alignment of the header.
    let text = unsafe { &*buffer.as_ptr().cast::<UNICODE_STRING>() };
    let allocation_start = buffer.as_ptr().addr();
    let text_start = text.Buffer.addr();
    let text_end = text_start.checked_add(usize::from(text.Length));
    let allocation_end = allocation_start + needed as usize;
    if text.Length % 2 != 0
        || text.MaximumLength % 2 != 0
        || text.MaximumLength < text.Length
        || text.MaximumLength - text.Length < 2
        || text_start % align_of::<u16>() != 0
        || text_start < allocation_start + size_of::<UNICODE_STRING>()
        || text_end
            .and_then(|end| end.checked_add(2))
            .is_none_or(|end| end > allocation_end)
        || text_start
            .checked_add(usize::from(text.MaximumLength))
            .is_none_or(|end| end > allocation_end)
    {
        bail!("invalid pipe server command-line information");
    }
    // SAFETY: the header's pointer, length and terminating unit are inside the
    // owned allocation and aligned. The next check verifies the promised NUL.
    let text = unsafe { std::slice::from_raw_parts(text.Buffer, usize::from(text.Length) / 2 + 1) };
    if text.last() != Some(&0) || text[..text.len() - 1].contains(&0) {
        bail!("invalid pipe server command-line termination");
    }
    let mut count = 0;
    // SAFETY: the validated UTF-16 command line has one terminal NUL. Successful
    // parsing returns a LocalAlloc-owned argv table and terminated argument strings.
    let arguments = unsafe { CommandLineToArgvW(text.as_ptr(), &mut count) };
    if arguments.is_null() {
        return Err(io::Error::last_os_error()).context("parse pipe server command line");
    }
    // SAFETY: count is supplied by the successful parser; short-circuit comparison
    // stops at the first mismatch, including an earlier terminating NUL.
    let server = unsafe {
        count > 1
            && "server"
                .encode_utf16()
                .chain(Some(0))
                .enumerate()
                .all(|(index, unit)| *(*arguments.add(1)).add(index) == unit)
    };
    // SAFETY: arguments uniquely owns the allocation returned by the parser.
    unsafe {
        LocalFree(arguments.cast());
    }
    Ok(server)
}
