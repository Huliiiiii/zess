use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Result;
use etcetera::BaseStrategy;

pub struct HostInfo {
    pub os: &'static str,
    pub arch: &'static str,
}

pub struct UserDirs {
    pub home: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub runtime: Option<PathBuf>,
}

pub trait Platform: Send + Sync {
    fn host_info(&self) -> HostInfo;
    fn user_dirs(&self) -> Result<UserDirs>;
    fn current_dir(&self) -> std::io::Result<PathBuf>;
    fn current_exe(&self) -> std::io::Result<PathBuf>;
    fn absolute_path(&self, path: &Path) -> std::io::Result<PathBuf>;
    fn process_id(&self) -> u32;
    fn system_time(&self) -> SystemTime;
    /// Returns an environment value exposed by this host provider.
    fn environment(&self, _name: &OsStr) -> Option<OsString> {
        None
    }
}

pub struct NativePlatform;

impl Platform for NativePlatform {
    fn host_info(&self) -> HostInfo {
        HostInfo {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        }
    }

    fn user_dirs(&self) -> Result<UserDirs> {
        let strategy = etcetera::choose_base_strategy()?;
        Ok(UserDirs {
            home: strategy.home_dir().to_owned(),
            config: strategy.config_dir(),
            data: strategy.data_dir(),
            cache: strategy.cache_dir(),
            runtime: strategy.runtime_dir(),
        })
    }

    fn current_dir(&self) -> std::io::Result<PathBuf> {
        std::env::current_dir()
    }

    fn current_exe(&self) -> std::io::Result<PathBuf> {
        std::env::current_exe()
    }

    fn absolute_path(&self, path: &Path) -> std::io::Result<PathBuf> {
        std::path::absolute(path)
    }

    fn process_id(&self) -> u32 {
        std::process::id()
    }

    fn system_time(&self) -> SystemTime {
        SystemTime::now()
    }

    fn environment(&self, name: &OsStr) -> Option<OsString> {
        std::env::var_os(name)
    }
}
