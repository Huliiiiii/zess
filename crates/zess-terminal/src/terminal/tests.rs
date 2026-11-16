use super::*;

#[cfg(target_os = "linux")]
mod pty;
#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
#[test]
fn a_failed_mouse_restore_still_releases_the_screen_and_paste_mode() {
    struct Writer {
        fail_next: bool,
        bytes: Vec<u8>,
    }
    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if std::mem::take(&mut self.fail_next) {
                Err(io::Error::other("output failed"))
            } else {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut screen = ScreenState::default();
    let mut writer = Writer {
        fail_next: false,
        bytes: vec![],
    };
    screen.activate(&mut writer).unwrap();
    screen.set_mouse_capture(&mut writer, true).unwrap();
    writer.bytes.clear();
    writer.fail_next = true;
    assert!(screen.restore(&mut writer).is_err());
    let output = String::from_utf8(writer.bytes.clone()).unwrap();
    assert!(output.contains("\x1b[?2004l"));
    assert!(output.contains("\x1b[?25h"));
    assert!(output.contains("\x1b[?1049l"));
    writer.bytes.clear();
    screen.restore(&mut writer).unwrap();
    assert_eq!(writer.bytes, b"\x1b[?1006l\x1b[?1002l");
}
