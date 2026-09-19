use crate::bootstrap;
use crate::cli::KickTarget;
use crate::commands::seedling_command_context;
use crate::util::{spawn_service, wait_until_running};
use config::DouglasFolders;
use file_system::{FileReader, UnixFileReader};
use os::{Os, Unix};
use std::path::Path;
use std::{process::ExitCode, sync::Arc};
use heartbeat::{HeartbeatReader, LocalHeartbeatReader};

pub(crate) async fn kick(kick_target: KickTarget) -> ExitCode {
    let (douglas_folders, guard) =
        seedling_command_context(&format!("Kicking target {kick_target}"));

    let heartbeat_file = douglas_folders.service_heartbeat_file(kick_target.service_name());
    let file_reader: Arc<dyn FileReader> = Arc::new(UnixFileReader::new());
    let os = Unix::new();

    if missing_heartbeat_file(&guard, kick_target, &heartbeat_file, &*file_reader) {
        guard.finish_with_outcome(log::Outcome::Failed);
        return ExitCode::from(1);
    }

    let Some(pid) = read_pid_from_heartbeat_file(&guard, kick_target, &heartbeat_file, file_reader)
    else {
        guard.finish_with_outcome(log::Outcome::Failed);
        return ExitCode::from(1);
    };

    let Some(is_running) = is_running(&guard, kick_target, &os, pid) else {
        guard.finish_with_outcome(log::Outcome::Failed);
        return ExitCode::from(1);
    };

    if is_running {
        if !send_kill_signal(&guard, kick_target, &os, pid) {
            guard.finish_with_outcome(log::Outcome::Failed);
            return ExitCode::from(1);
        }
    } else {
        guard.span().message(
            log::Level::Info,
            &format!("Service {kick_target} is not running"),
        );
    }

    if start_service(&guard, kick_target, &os, &douglas_folders).await {
        ExitCode::from(0)
    } else {
        guard.finish_with_outcome(log::Outcome::Failed);
        ExitCode::from(1)
    }
}

async fn start_service(
    guard: &log::ScopeGuard,
    kick_target: KickTarget,
    os: &dyn Os,
    douglas_folders: &DouglasFolders,
) -> bool {
    match spawn_service(kick_target.service_name(), None, true, os, guard.span()) {
        Ok(()) => {
            let liveness = match bootstrap::system::liveness_check(
                kick_target.service_name(),
                douglas_folders,
            ) {
                Ok(liveness) => liveness,
                Err(err) => {
                    guard.span().message(
                        log::Level::Warn,
                        &format!("Could not determine liveness check for {kick_target}: {err}"),
                    );
                    return false;
                }
            };

            if wait_until_running(&liveness, guard.span()).await {
                guard.span().message(
                    log::Level::Info,
                    &format!("Service {kick_target} restarted"),
                );
                return true;
            }
            guard.span().message(
                log::Level::Warn,
                &format!("Service {kick_target} did not become ready in time"),
            );
            false
        }
        Err(err) => {
            guard.span().message(
                log::Level::Warn,
                &format!("Error kicking {kick_target}: {err}"),
            );
            false
        }
    }
}

fn send_kill_signal(
    guard: &log::ScopeGuard,
    kick_target: KickTarget,
    os: &dyn Os,
    pid: u32,
) -> bool {
    match os.kill(pid) {
        Ok(()) => {
            guard
                .span()
                .message(log::Level::Info, &format!("Stopped service {kick_target}"));
            true
        }
        Err(err) => {
            guard.span().message(
                log::Level::Warn,
                &format!("Error kicking {kick_target}: {err}"),
            );
            false
        }
    }
}

fn is_running(
    guard: &log::ScopeGuard,
    kick_target: KickTarget,
    os: &dyn Os,
    pid: u32,
) -> Option<bool> {
    match os.is_active_pid(pid) {
        Ok(state) => Some(state),
        Err(err) => {
            guard.span().message(
                log::Level::Warn,
                &format!("Could not determine if {kick_target} is running: {err}"),
            );
            None
        }
    }
}

fn missing_heartbeat_file(
    guard: &log::ScopeGuard,
    kick_target: KickTarget,
    heartbeat_file: &Path,
    file_reader: &dyn FileReader,
) -> bool {
    if file_reader.exists(heartbeat_file) {
        return false;
    }

    guard.span().message(
        log::Level::Warn,
        &format!("No heartbeat file for {kick_target}"),
    );

    true
}

fn read_pid_from_heartbeat_file(
    guard: &log::ScopeGuard,
    kick_target: KickTarget,
    heartbeat_file: &Path,
    file_reader: Arc<dyn FileReader>,
) -> Option<u32> {
    let local_heartbeat_reader = LocalHeartbeatReader::new(heartbeat_file, file_reader);
    match local_heartbeat_reader.read() {
        Ok(heartbeat) => Some(heartbeat.pid),
        Err(err) => {
            guard.span().message(
                log::Level::Warn,
                &format!("Error inspecting {kick_target}: {err}"),
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::MockFileReader;
    use os::MockOs;
    use std::sync::Arc;
    use heartbeat::Heartbeat;

    struct NullReporter;

    impl log::Reporter for NullReporter {
        fn emit(&self, _event: log::Event) {}
    }

    fn test_guard() -> log::ScopeGuard {
        log::Span::new(Arc::new(NullReporter), "test", log::ScopeKind::Task).start_guard()
    }

    #[test]
    fn test_missing_heartbeat_file_should_be_false_when_the_file_exists() {
        let guard = test_guard();
        let mut file_reader = MockFileReader::new();
        file_reader.expect_exists().returning(|_| true);

        let result = missing_heartbeat_file(
            &guard,
            KickTarget::Bract,
            Path::new("/tmp/bract.heartbeat"),
            &file_reader,
        );

        assert!(!result);
    }

    #[test]
    fn test_missing_heartbeat_file_should_be_true_when_the_file_is_absent() {
        let guard = test_guard();
        let mut file_reader = MockFileReader::new();
        file_reader.expect_exists().returning(|_| false);

        let result = missing_heartbeat_file(
            &guard,
            KickTarget::Bract,
            Path::new("/tmp/bract.heartbeat"),
            &file_reader,
        );

        assert!(result);
    }

    #[test]
    fn test_read_pid_from_heartbeat_file_should_return_the_stored_pid() {
        let guard = test_guard();
        let heartbeat = Heartbeat {
            pid: 4242,
            written_at: std::time::SystemTime::UNIX_EPOCH,
        };
        let Ok(serialized) = serde_json::to_string(&heartbeat) else {
            panic!("should serialize the heartbeat");
        };

        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_all()
            .returning(move |_| Ok(serialized.clone()));

        let result = read_pid_from_heartbeat_file(
            &guard,
            KickTarget::Bract,
            Path::new("/tmp/bract.heartbeat"),
            Arc::new(file_reader),
        );

        assert_eq!(result, Some(4242));
    }

    #[test]
    fn test_read_pid_from_heartbeat_file_should_return_none_when_the_contents_are_invalid() {
        let guard = test_guard();
        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_read_all()
            .returning(|_| Ok("not valid json".to_string()));

        let result = read_pid_from_heartbeat_file(
            &guard,
            KickTarget::Bract,
            Path::new("/tmp/bract.heartbeat"),
            Arc::new(file_reader),
        );

        assert_eq!(result, None);
    }

    #[test]
    fn test_is_running_should_be_some_true_when_the_pid_is_active() {
        let guard = test_guard();
        let mut os = MockOs::new();
        os.expect_is_active_pid().returning(|_| Ok(true));

        let result = is_running(&guard, KickTarget::Bract, &os, 4242);

        assert_eq!(result, Some(true));
    }

    #[test]
    fn test_is_running_should_be_some_false_when_the_pid_is_inactive() {
        let guard = test_guard();
        let mut os = MockOs::new();
        os.expect_is_active_pid().returning(|_| Ok(false));

        let result = is_running(&guard, KickTarget::Bract, &os, 4242);

        assert_eq!(result, Some(false));
    }

    #[test]
    fn test_is_running_should_be_none_when_the_os_check_fails() {
        let guard = test_guard();
        let mut os = MockOs::new();
        os.expect_is_active_pid()
            .returning(|_| Err(os::OsError::NoSuchPid(4242)));

        let result = is_running(&guard, KickTarget::Bract, &os, 4242);

        assert_eq!(result, None);
    }

    #[test]
    fn test_send_kill_signal_should_be_true_when_the_kill_succeeds() {
        let guard = test_guard();
        let mut os = MockOs::new();
        os.expect_kill().returning(|_| Ok(()));

        let result = send_kill_signal(&guard, KickTarget::Bract, &os, 4242);

        assert!(result);
    }

    #[test]
    fn test_send_kill_signal_should_be_false_when_the_kill_fails() {
        let guard = test_guard();
        let mut os = MockOs::new();
        os.expect_kill()
            .returning(|_| Err(os::OsError::NoSuchPid(4242)));

        let result = send_kill_signal(&guard, KickTarget::Bract, &os, 4242);

        assert!(!result);
    }
}
