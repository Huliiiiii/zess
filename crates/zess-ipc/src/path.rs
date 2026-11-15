use std::{fmt, str::FromStr};

use anyhow::{Result, bail};

pub struct ServerId(pub(crate) u64);

impl ServerId {
    pub fn for_name(name: &str) -> Self {
        let mut hash = xxhash_rust::xxh3::Xxh3::new();
        hash.update(b"zess session name\0");
        hash.update(name.as_bytes());
        Self(hash.digest())
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

impl FromStr for ServerId {
    type Err = anyhow::Error;

    fn from_str(id: &str) -> Result<Self> {
        if id.len() != 16 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("server ID must be the 16-digit hexadecimal ID from `zess session list`");
        }
        Ok(Self(u64::from_str_radix(id, 16)?))
    }
}

#[cfg(unix)]
#[path = "unix/path.rs"]
mod backend;
#[cfg(windows)]
#[path = "windows/path.rs"]
mod backend;

pub use backend::SocketPath;
