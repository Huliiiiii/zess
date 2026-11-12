use std::{fmt, str::FromStr};

use anyhow::{Result, bail};

pub struct ServerId(pub(crate) u64);

impl fmt::Display for ServerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

impl FromStr for ServerId {
    type Err = anyhow::Error;

    fn from_str(id: &str) -> Result<Self> {
        if id.len() != 16 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("server ID must be the 16-digit hexadecimal ID from `zess server list`");
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
