use std::{
    fmt::Write as _,
    fs::File,
    io::{self, Read},
    path::Path,
    sync::LazyLock,
};

use sha2::{Digest, Sha256};

pub const RELEASE_VERSION: Option<&str> = option_env!("ZESS_RELEASE_VERSION");

pub fn development_version(executable: &Path) -> io::Result<String> {
    let mut file = File::open(executable)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let mut version = String::with_capacity(68);
    version.push_str("dev-");
    for byte in hash.finalize() {
        write!(version, "{byte:02x}").unwrap();
    }
    Ok(version)
}

pub fn version() -> &'static str {
    static DEVELOPMENT: LazyLock<String> = LazyLock::new(|| {
        std::env::current_exe()
            .and_then(|path| development_version(&path))
            .expect("read running Zess executable to identify the development build")
    });
    RELEASE_VERSION.unwrap_or_else(|| DEVELOPMENT.as_str())
}
