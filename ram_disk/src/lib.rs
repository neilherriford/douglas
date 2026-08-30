mod linux_ram_disk;
mod macos_ram_disk;

#[cfg(target_os = "linux")]
use crate::linux_ram_disk::LinuxRamDisk;
#[cfg(target_os = "macos")]
use crate::macos_ram_disk::MacosRamDisk;
#[cfg(feature = "mock")]
use mockall::automock;
use nix::sys::stat::stat;
use os::{Os, OsError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RamDiskError {
    #[error("OS error: {0}")]
    Os(#[from] OsError),
    #[error("Could not stat '{path}': {message}")]
    StatFailed { path: PathBuf, message: String },
    #[error("Could not parse device path from hdiutil output: '{0}'")]
    UnparseableHdiutilOutput(String),
}

#[cfg_attr(feature = "mock", automock)]
pub trait RamDisk: Send + Sync {
    fn mount(&self, path: &Path, size_mb: u32) -> Result<bool, RamDiskError>;
    fn unmount(&self, path: &Path) -> Result<(), RamDiskError>;
    fn is_mounted(&self, path: &Path) -> Result<bool, RamDiskError>;
}

pub(crate) fn is_distinct_mount_point(path: &Path) -> Result<bool, RamDiskError> {
    if !path.exists() {
        return Ok(false);
    }
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    if !parent.exists() {
        return Ok(false);
    }

    let path_stat = stat(path).map_err(|errno| RamDiskError::StatFailed {
        path: path.to_path_buf(),
        message: errno.to_string(),
    })?;
    let parent_stat = stat(parent).map_err(|errno| RamDiskError::StatFailed {
        path: parent.to_path_buf(),
        message: errno.to_string(),
    })?;

    Ok(path_stat.st_dev != parent_stat.st_dev)
}

#[cfg(target_os = "linux")]
pub fn create_ram_disk(os: Arc<dyn Os>) -> Box<dyn RamDisk> {
    Box::new(LinuxRamDisk::new(os))
}

#[cfg(target_os = "macos")]
pub fn create_ram_disk(os: Arc<dyn Os>) -> Box<dyn RamDisk> {
    Box::new(MacosRamDisk::new(os))
}
