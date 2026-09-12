use crate::cli_reporter::CliReporter;
use config::DouglasFolders;
use log::{BufferedFileReporter, Reporter, TeeReporter};
use std::{io, path::Path, process::ExitCode, sync::Arc};

pub(crate) fn open_daemon_crash_log(path: &Path) -> Option<(std::fs::File, std::fs::File)> {
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "Failed to create crash log directory '{}': {err}",
            parent.display()
        );
        return None;
    }

    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .inspect_err(|err| eprintln!("Failed to open crash log '{}': {err}", path.display()))
        .ok()?;
    let stderr = stdout.try_clone().ok()?;
    Some((stdout, stderr))
}

pub(crate) fn initgroups_for(user_name: &str) {
    if let Err(err) = try_initgroups_for(user_name) {
        eprintln!("Failed to initialize supplementary groups for '{user_name}': {err}");
    }
}

fn try_initgroups_for(user_name: &str) -> Result<(), String> {
    let Some(user) = users::get_user_by_name(user_name) else {
        return Err(format!("user '{user_name}' not found"));
    };
    let c_user = std::ffi::CString::new(user_name).map_err(|err| err.to_string())?;
    let gid = nix::unistd::Gid::from_raw(user.primary_group_id());

    platform_initgroups(&c_user, gid).map_err(|err| err.to_string())
}

#[cfg(target_os = "linux")]
fn platform_initgroups(user: &std::ffi::CStr, gid: nix::unistd::Gid) -> nix::Result<()> {
    nix::unistd::initgroups(user, gid)
}

#[cfg(target_os = "macos")]
#[allow(clippy::unnecessary_wraps)]
fn platform_initgroups(_user: &std::ffi::CStr, _gid: nix::unistd::Gid) -> nix::Result<()> {
    Ok(())
}

pub(crate) fn run_with_tokio(fut: impl std::future::Future<Output = ExitCode>) -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(fut),
        Err(err) => {
            eprintln!("Failed to start async runtime: {err}");
            ExitCode::from(1)
        }
    }
}

pub(crate) fn build_cli_reporter(
    douglas_folders: &DouglasFolders,
    log_name: &str,
) -> io::Result<Arc<dyn Reporter>> {
    let cli_reporter = CliReporter::start()?;
    Ok(Arc::new(TeeReporter::new(vec![
        Box::new(BufferedFileReporter::new(
            douglas_folders.service_log_file(log_name),
        )),
        Box::new(cli_reporter),
    ])))
}

pub(crate) fn build_plain_reporter(
    douglas_folders: &DouglasFolders,
    log_name: &str,
) -> Arc<dyn Reporter> {
    Arc::new(BufferedFileReporter::new(
        douglas_folders.service_log_file(log_name),
    ))
}

#[cfg(test)]
mod crash_log_tests {
    use super::open_daemon_crash_log;
    use std::io::Read;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "douglas-crash-log-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        dir
    }

    #[test]
    fn test_open_daemon_crash_log_should_create_a_missing_parent_directory() {
        let dir = unique_temp_dir();
        let mut path = dir.clone();
        path.push("nested");
        path.push("resin.crash.log");
        assert!(!dir.exists());

        let result = open_daemon_crash_log(&path);

        assert!(result.is_some());
        assert!(path.exists());

        let Ok(()) = std::fs::remove_dir_all(&dir) else {
            panic!("cleanup should succeed");
        };
    }

    #[test]
    fn test_open_daemon_crash_log_should_open_the_same_file_for_stdout_and_stderr() {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;

        let dir = unique_temp_dir();
        let Ok(()) = std::fs::create_dir_all(&dir) else {
            panic!("setup should succeed");
        };
        let mut path = dir.clone();
        path.push("resin.crash.log");

        let Some((mut stdout, mut stderr)) = open_daemon_crash_log(&path) else {
            panic!("should open crash log");
        };

        assert_ne!(stdout.as_raw_fd(), stderr.as_raw_fd());
        let Ok(()) = stdout.write_all(b"hello ") else {
            panic!("write should succeed");
        };
        let Ok(()) = stderr.write_all(b"world") else {
            panic!("write should succeed");
        };

        let mut contents = String::new();
        let Ok(mut file) = std::fs::File::open(&path) else {
            panic!("file should exist");
        };
        let Ok(_) = file.read_to_string(&mut contents) else {
            panic!("read should succeed");
        };
        assert_eq!(contents, "hello world");

        let Ok(()) = std::fs::remove_dir_all(&dir) else {
            panic!("cleanup should succeed");
        };
    }

    #[test]
    fn test_open_daemon_crash_log_should_append_across_repeated_opens() {
        let dir = unique_temp_dir();
        let Ok(()) = std::fs::create_dir_all(&dir) else {
            panic!("setup should succeed");
        };
        let mut path = dir.clone();
        path.push("resin.crash.log");

        {
            use std::io::Write;
            let Some((mut stdout, _stderr)) = open_daemon_crash_log(&path) else {
                panic!("should open crash log");
            };
            let Ok(()) = stdout.write_all(b"first boot\n") else {
                panic!("write should succeed");
            };
        }
        {
            use std::io::Write;
            let Some((mut stdout, _stderr)) = open_daemon_crash_log(&path) else {
                panic!("should open crash log");
            };
            let Ok(()) = stdout.write_all(b"second boot\n") else {
                panic!("write should succeed");
            };
        }

        let Ok(contents) = std::fs::read_to_string(&path) else {
            panic!("read should succeed");
        };
        assert_eq!(contents, "first boot\nsecond boot\n");

        let Ok(()) = std::fs::remove_dir_all(&dir) else {
            panic!("cleanup should succeed");
        };
    }
}
