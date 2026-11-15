use std::{os::windows::io::AsRawHandle, path::PathBuf, ptr::null_mut};

use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        ACCESS_ALLOWED_ACE,
        Authorization::{ConvertStringSidToSidW, GetSecurityInfo, SE_KERNEL_OBJECT},
        DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    },
};

use crate::{Listener, SocketPath, Stream, windows::security};

#[test]
fn the_pipe_acl_grants_access_only_to_the_current_user() {
    let socket = SocketPath::from(PathBuf::from(format!(
        "zess-acl-test-{}",
        std::process::id()
    )));
    let listener = Listener::bind(&socket).unwrap();
    let client = Stream::connect(&socket).unwrap();
    let _server = listener.accept().unwrap();
    let pipe = client.handle().unwrap();
    let mut descriptor = null_mut();
    let mut dacl = null_mut();
    // SAFETY: pipe is live and output pointers are writable. The successful
    // descriptor is LocalAlloc-owned; dacl and ACE pointers borrow it.
    assert_eq!(
        unsafe {
            GetSecurityInfo(
                pipe.as_raw_handle(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        },
        0
    );
    assert!(!dacl.is_null());
    let sid = security::current_user_sid()
        .unwrap()
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut expected_sid = null_mut();
    // SAFETY: sid is NUL-terminated; output is independently LocalAlloc-owned.
    assert_ne!(
        unsafe { ConvertStringSidToSidW(sid.as_ptr(), &mut expected_sid) },
        0
    );
    // SAFETY: the operating system returned a valid ACL. GetAce returns a borrow
    // into descriptor; the access-allowed type is checked before reading its SID.
    let (count, ace_type, same_user) = unsafe {
        let count = (*dacl).AceCount;
        let mut ace = null_mut();
        assert_ne!(GetAce(dacl, 0, &mut ace), 0);
        let ace = &*ace.cast::<ACCESS_ALLOWED_ACE>();
        let ace_type = ace.Header.AceType;
        let same_user = ace_type == 0
            && EqualSid(
                (&ace.SidStart as *const u32).cast_mut().cast(),
                expected_sid,
            ) != 0;
        LocalFree(expected_sid);
        LocalFree(descriptor);
        (count, ace_type, same_user)
    };
    assert_eq!(count, 1);
    assert_eq!(ace_type, 0);
    assert!(same_user);
}

#[test]
fn forced_stop_refuses_to_terminate_its_own_process() {
    use crate::{
        NativeLocalIpc,
        platform::{DaemonControl, NativeDaemonControl},
    };

    let socket = SocketPath::from(PathBuf::from(format!(
        "zess-self-stop-test-{}",
        std::process::id()
    )));
    let _listener = Listener::bind(&socket).unwrap();
    let (_stream, process) = NativeDaemonControl
        .connect_for_stop(&NativeLocalIpc, &socket)
        .unwrap();
    let error = process.terminate().unwrap_err();
    assert!(error.to_string().contains("invalid server process"));
}
