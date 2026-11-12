use std::{
    fs,
    mem::{size_of, zeroed},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    ptr::null,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use windows_sys::Win32::{
    Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
            JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, SetInformationJobObject,
        },
        Threading::{
            CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS, OpenProcess, PROCESS_QUERY_INFORMATION,
            PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
        },
    },
};
use zess_ipc::platform::{DaemonControl, DaemonSpec, NativeDaemonControl};

struct Launcher {
    root: PathBuf,
    job: Option<OwnedHandle>,
    process: Child,
}

impl Launcher {
    fn new(breakaway: u32) -> Self {
        let root = std::env::temp_dir().join(format!(
            "zess-job-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        // SAFETY: no name/security attributes are passed; successful handle is owned.
        let raw = unsafe { CreateJobObjectW(null(), null()) };
        assert!(!raw.is_null());
        // SAFETY: successful CreateJobObjectW transferred unique ownership.
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: each field has a valid all-zero representation.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | breakaway;
        // SAFETY: job is live; limits has the declared size and initialized fields.
        assert_ne!(
            unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            },
            0
        );
        let log = fs::File::create(root.join("launcher.log")).unwrap();
        let process = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "windows::job_launcher_child",
                "--ignored",
                "--nocapture",
            ])
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        // The launcher waits for its gate before spawning any children, so the
        // job association is established before testing inherited membership.
        // SAFETY: both handles remain owned and refer to this test's objects.
        assert_ne!(
            unsafe { AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) },
            0
        );
        fs::write(root.join("launch"), b"start").unwrap();
        Self {
            root,
            job: Some(job),
            process,
        }
    }

    fn result(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.root.join("ready").exists() {
            assert!(
                self.process.try_wait().unwrap().is_none(),
                "launcher failed: {}",
                fs::read_to_string(self.root.join("launcher.log")).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "launcher did not finish spawning"
            );
            thread::sleep(Duration::from_millis(10));
        }
        fs::read_to_string(self.root.join("result")).unwrap()
    }

    fn close_job(&mut self) {
        self.job.take();
    }
}

impl Drop for Launcher {
    fn drop(&mut self) {
        self.close_job();
        let deadline = Instant::now() + Duration::from_secs(12);
        while self.process.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                self.process.kill().unwrap();
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.process.wait().unwrap();
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct Process(OwnedHandle);

impl Process {
    fn open(pid: &str) -> Self {
        // SAFETY: pid belongs to a helper spawned by this test. No handle aliases
        // are created; OpenProcess grants an independent handle to that process.
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                0,
                pid.parse().unwrap(),
            )
        };
        assert!(!raw.is_null());
        // SAFETY: successful OpenProcess transferred unique ownership.
        Self(unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    fn in_job(&self, launcher: &Launcher) -> bool {
        let mut in_job = 0;
        // SAFETY: both handles are live and the output is writable.
        assert_ne!(
            unsafe {
                IsProcessInJob(
                    self.0.as_raw_handle(),
                    launcher.job.as_ref().unwrap().as_raw_handle(),
                    &mut in_job,
                )
            },
            0
        );
        in_job != 0
    }

    fn wait(&self, milliseconds: u32) -> u32 {
        // SAFETY: the owned handle has SYNCHRONIZE rights and stays live.
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), milliseconds) }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.wait(0) == WAIT_TIMEOUT {
            // SAFETY: this handle identifies only a child of this test, and has
            // TERMINATE access. Keep it until termination has completed.
            assert_ne!(unsafe { TerminateProcess(self.0.as_raw_handle(), 1) }, 0);
            assert_eq!(self.wait(2000), WAIT_OBJECT_0);
        }
    }
}

#[test]
fn the_daemon_survives_closing_an_ssh_style_parent_job() {
    let mut launcher = Launcher::new(JOB_OBJECT_LIMIT_BREAKAWAY_OK);
    let result = launcher.result();
    let (control, daemon) = result.split_once('\n').unwrap();
    let control = Process::open(control);
    let daemon = Process::open(daemon.strip_prefix("daemon ").expect(&result));
    assert!(control.in_job(&launcher));
    assert!(!daemon.in_job(&launcher));
    launcher.close_job();
    assert_eq!(control.wait(2000), WAIT_OBJECT_0);
    assert_eq!(daemon.wait(100), WAIT_TIMEOUT);
}

#[test]
fn silent_breakaway_keeps_the_daemon_out_of_the_parent_job() {
    let mut launcher = Launcher::new(JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK);
    let result = launcher.result();
    let (control, daemon) = result.split_once('\n').unwrap();
    let control = Process::open(control);
    let daemon = Process::open(daemon.strip_prefix("daemon ").expect(&result));
    assert!(!control.in_job(&launcher));
    assert!(!daemon.in_job(&launcher));
    launcher.close_job();
    assert_eq!(daemon.wait(100), WAIT_TIMEOUT);
}

#[test]
fn a_host_job_that_forbids_breakaway_reports_an_error() {
    let mut launcher = Launcher::new(0);
    let result = launcher.result();
    let (control, daemon) = result.split_once('\n').unwrap();
    let control = Process::open(control);
    assert!(control.in_job(&launcher));
    assert!(
        daemon.starts_with("error ") && daemon.contains("does not allow breakaway"),
        "{result}"
    );
}

#[test]
#[ignore = "child process entry point for the Job Object lifetime tests"]
fn job_launcher_child() {
    let root = std::env::current_dir().unwrap();
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("zess-job-test-")
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("launch").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let mut control = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "windows::job_process_child",
            "--ignored",
            "--nocapture",
        ])
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .unwrap();
    let daemon = NativeDaemonControl.spawn(&DaemonSpec {
        program: std::env::current_exe().unwrap(),
        args: [
            "--exact",
            "windows::job_process_child",
            "--ignored",
            "--nocapture",
        ]
        .map(Into::into)
        .into(),
        cwd: Some(root.clone()),
        log_path: root.join("daemon.log"),
    });
    let daemon = match daemon {
        Ok(daemon) => format!("daemon {}", daemon.id()),
        Err(error) => format!("error {error:#}"),
    };
    fs::write(root.join("result"), format!("{}\n{daemon}", control.id())).unwrap();
    fs::write(root.join("ready"), b"ready").unwrap();
    control.wait().unwrap();
}

#[test]
#[ignore = "child process entry point for the Job Object lifetime tests"]
fn job_process_child() {
    assert!(
        std::env::current_dir()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("zess-job-test-")
    );
    thread::sleep(Duration::from_secs(10));
}
