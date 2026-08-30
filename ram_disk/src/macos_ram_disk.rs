use crate::{RamDisk, RamDiskError, is_distinct_mount_point};
use os::Os;
use std::path::Path;
use std::sync::Arc;

const VOLUME_NAME: &str = "douglas-ramdisk";

pub(crate) struct MacosRamDisk {
    os: Arc<dyn Os>,
}

impl MacosRamDisk {
    pub(crate) fn new(os: Arc<dyn Os>) -> Self {
        Self { os }
    }

    fn attach(&self, size_mb: u32) -> Result<String, RamDiskError> {
        let sectors = u64::from(size_mb) * 2048;
        let output = self.os.execute_with_output(
            "hdiutil",
            vec![
                "attach".to_string(),
                "-nomount".to_string(),
                format!("ram://{sectors}"),
            ],
            Vec::new(),
        )?;

        Self::parse_attached_device(&output.stdout)
    }

    fn parse_attached_device(stdout: &[u8]) -> Result<String, RamDiskError> {
        let text = String::from_utf8_lossy(stdout);
        text.lines()
            .find_map(|line| line.split_whitespace().next())
            .map(std::string::ToString::to_string)
            .ok_or_else(|| RamDiskError::UnparseableHdiutilOutput(text.trim().to_string()))
    }

    fn detach(&self, device: &str) -> Result<(), RamDiskError> {
        self.os
            .execute_with_output(
                "hdiutil",
                vec!["detach".to_string(), device.to_string()],
                Vec::new(),
            )
            .map(|_| ())?;

        Ok(())
    }

    fn find_attached_device(&self, path: &Path) -> Result<Option<String>, RamDiskError> {
        let path = path.to_string_lossy().to_string();
        let output = self
            .os
            .execute_with_output("mount", Vec::new(), Vec::new())?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        Ok(stdout.lines().find_map(|line| {
            let (device, rest) = line.split_once(" on ")?;
            let mount_point = rest.split(" (").next().unwrap_or(rest);
            (mount_point == path).then(|| device.trim().to_string())
        }))
    }
}

impl RamDisk for MacosRamDisk {
    fn mount(&self, path: &Path, size_mb: u32) -> Result<bool, RamDiskError> {
        if self.is_mounted(path)? {
            return Ok(true);
        }

        let device = self.attach(size_mb)?;

        if let Err(err) = self.os.execute_with_output(
            "newfs_hfs",
            vec!["-v".to_string(), VOLUME_NAME.to_string(), device.clone()],
            Vec::new(),
        ) {
            let _ = self.detach(&device);
            return Err(err.into());
        }

        if let Err(err) = self.os.execute_with_output(
            "mount",
            vec![
                "-t".to_string(),
                "hfs".to_string(),
                device.clone(),
                path.to_string_lossy().to_string(),
            ],
            Vec::new(),
        ) {
            let _ = self.detach(&device);
            return Err(err.into());
        }

        Ok(true)
    }

    fn unmount(&self, path: &Path) -> Result<(), RamDiskError> {
        if !self.is_mounted(path)? {
            return Ok(());
        }

        let device = self.find_attached_device(path)?;

        self.os
            .execute_with_output(
                "umount",
                vec![path.to_string_lossy().to_string()],
                Vec::new(),
            )
            .map(|_| ())?;

        if let Some(device) = device {
            self.detach(&device)?;
        }

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
        let ram_disk = MacosRamDisk::new(Arc::new(os));

        let result = ram_disk.is_mounted(&dir);

        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn test_unmount_should_no_op_when_the_path_is_not_mounted() {
        let dir = std::env::temp_dir().join("ram-disk-macos-test-not-mounted");
        let os = MockOs::new();
        let ram_disk = MacosRamDisk::new(Arc::new(os));

        let result = ram_disk.unmount(&dir);

        assert!(matches!(result, Ok(())));
    }

    #[test]
    fn test_parse_attached_device_should_take_the_first_token_of_the_first_line() {
        let stdout = b"/dev/disk4              \n";

        let result = MacosRamDisk::parse_attached_device(stdout);

        assert!(matches!(result, Ok(device) if device == "/dev/disk4"));
    }

    #[test]
    fn test_parse_attached_device_should_fail_on_empty_output() {
        let result = MacosRamDisk::parse_attached_device(b"");

        assert!(matches!(
            result,
            Err(RamDiskError::UnparseableHdiutilOutput(_))
        ));
    }

    #[test]
    fn test_find_attached_device_should_match_the_exact_mount_point() {
        let mut os = MockOs::new();
        os.expect_execute_with_output()
            .withf(|command, _args, _env| command == "mount")
            .times(1)
            .returning(|_, _, _| {
                Ok(std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(0),
                    stdout: b"/dev/disk4 on /var/lib/douglas/mounts/secrets/openbao-agent (hfs, local, nodev, nosuid, journaled)\n".to_vec(),
                    stderr: Vec::new(),
                })
            });
        let ram_disk = MacosRamDisk::new(Arc::new(os));

        let result = ram_disk
            .find_attached_device(Path::new("/var/lib/douglas/mounts/secrets/openbao-agent"));

        assert!(matches!(result, Ok(Some(device)) if device == "/dev/disk4"));
    }
}
