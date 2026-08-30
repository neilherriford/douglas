use crate::{RamDisk, RamDiskError, is_distinct_mount_point};
use os::Os;
use std::path::Path;
use std::sync::Arc;

pub(crate) struct LinuxRamDisk {
    os: Arc<dyn Os>,
}

impl LinuxRamDisk {
    #![allow(dead_code)]
    pub(crate) fn new(os: Arc<dyn Os>) -> Self {
        Self { os }
    }

    fn mount_with_options(&self, path: &Path, options: &str) -> Result<(), os::OsError> {
        self.os
            .execute_with_output(
                "mount",
                vec![
                    "-t".to_string(),
                    "tmpfs".to_string(),
                    "-o".to_string(),
                    options.to_string(),
                    "tmpfs".to_string(),
                    path.to_string_lossy().to_string(),
                ],
                Vec::new(),
            )
            .map(|_| ())
    }
}

impl RamDisk for LinuxRamDisk {
    fn mount(&self, path: &Path, size_mb: u32) -> Result<bool, RamDiskError> {
        if self.is_mounted(path)? {
            return Ok(true);
        }

        if self
            .mount_with_options(path, &format!("size={size_mb}m,mode=0700,noswap"))
            .is_ok()
        {
            return Ok(true);
        }

        self.mount_with_options(path, &format!("size={size_mb}m,mode=0700"))?;
        Ok(false)
    }

    fn unmount(&self, path: &Path) -> Result<(), RamDiskError> {
        if !self.is_mounted(path)? {
            return Ok(());
        }

        self.os
            .execute_with_output(
                "umount",
                vec![path.to_string_lossy().to_string()],
                Vec::new(),
            )
            .map(|_| ())?;

        Ok(())
    }

    fn is_mounted(&self, path: &Path) -> Result<bool, RamDiskError> {
        is_distinct_mount_point(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use os::MockOs;

    #[test]
    fn test_is_mounted_should_be_false_for_an_ordinary_directory() {
        let dir = std::env::temp_dir();
        let os = MockOs::new();
        let ram_disk = LinuxRamDisk::new(Arc::new(os));

        let result = ram_disk.is_mounted(&dir);

        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_mount_should_fall_back_without_noswap_when_the_first_attempt_fails() {
        let dir = std::env::temp_dir().join("ram-disk-linux-test-nonexistent-child");
        let mut os = MockOs::new();
        os.expect_execute_with_output()
            .withf(|command, args, _env| {
                command == "mount" && args.iter().any(|arg| arg.contains("noswap"))
            })
            .times(1)
            .returning(|_, _, _| {
                Err(os::OsError::ProccessExitStatusError {
                    name: "mount".to_string(),
                    code: Some(1),
                    args: Vec::new(),
                })
            });
        os.expect_execute_with_output()
            .withf(|command, args, _env| {
                command == "mount" && !args.iter().any(|arg| arg.contains("noswap"))
            })
            .times(1)
            .returning(|_, _, _| {
                Ok(std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(0),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                })
            });
        let ram_disk = LinuxRamDisk::new(Arc::new(os));

        let result = ram_disk.mount(&dir, 1);

        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_unmount_should_no_op_when_the_path_is_not_mounted() {
        let dir = std::env::temp_dir().join("ram-disk-linux-test-not-mounted");
        let os = MockOs::new();
        let ram_disk = LinuxRamDisk::new(Arc::new(os));

        let result = ram_disk.unmount(&dir);

        assert!(matches!(result, Ok(())));
    }
}
