use crate::RunningStatus;
use file_system::{
    BindableUnixDomainSocketFile, FileDeleter, FileSystemError, Listener, Modes, Permissions,
    UnixFileReader,
};
use heartbeat::{HeartbeatReader, HeartbeatReaderError, LocalHeartbeatReader};
use log::{Level, Outcome, ScopeKind, Span};
use os::{Os, Unix};
use std::io::ErrorKind;
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug)]
pub struct ListenerDefinition {
    pub socket_path: PathBuf,
    pub owning_user: String,
    pub owning_group: String,
    pub mode: Modes,
}

impl ListenerDefinition {
    pub fn new(socket_path: &Path, owning_user: &str, owning_group: &str, mode: Modes) -> Self {
        Self {
            socket_path: socket_path.to_path_buf(),
            owning_user: owning_user.to_string(),
            owning_group: owning_group.to_string(),
            mode,
        }
    }
}

pub struct SocketListenerFactory {
    definition: ListenerDefinition,
    file_deleter: Arc<dyn FileDeleter>,
    permissions: Arc<dyn Permissions>,
    bindable_unix_domain_socket_file: Arc<dyn BindableUnixDomainSocketFile>,
}

impl SocketListenerFactory {
    pub fn new(
        definition: ListenerDefinition,
        file_deleter: Arc<dyn FileDeleter>,
        permissions: Arc<dyn Permissions>,
        bindable_unix_domain_socket_file: Arc<dyn BindableUnixDomainSocketFile>,
    ) -> Self {
        Self {
            definition,
            file_deleter,
            permissions,
            bindable_unix_domain_socket_file,
        }
    }

    pub fn create(
        &self,
        span: &Span,
    ) -> Result<Box<dyn Listener + Send + Sync + 'static>, FileSystemError> {
        let child_span = span.create_child(
            &format!(
                "Refreshing socket '{}'",
                self.definition.socket_path.to_string_lossy()
            ),
            ScopeKind::Step,
        );
        let log = child_span.create_scoped_reporter();
        self.file_deleter.delete(&self.definition.socket_path)?;

        let listener = self
            .bindable_unix_domain_socket_file
            .bind(&self.definition.socket_path)?;

        self.permissions.change_user_and_group_ownership(
            &self.definition.socket_path,
            &self.definition.owning_user,
            &self.definition.owning_group,
        )?;
        self.permissions
            .change_mode(&self.definition.socket_path, &self.definition.mode)?;

        log.finish(Outcome::Ok);
        Ok(listener)
    }
}

#[derive(Debug, Clone)]
pub enum LivenessCheck {
    UnixSocket(PathBuf),
    TcpPort { host: String, port: u16 },
    Heartbeat { path: PathBuf, max_age: Duration },
}

pub fn check_liveness(span: &Span, check: &LivenessCheck) -> RunningStatus {
    match check {
        LivenessCheck::UnixSocket(socket_path) => check_unix_socket(span, socket_path),
        LivenessCheck::TcpPort { host, port } => check_tcp_port(span, host, *port),
        LivenessCheck::Heartbeat { path, max_age } => check_heartbeat(span, path, *max_age),
    }
}

// UDS presence can only be verified by trying to connect to it
fn check_unix_socket(span: &Span, socket_path: &Path) -> RunningStatus {
    match UnixStream::connect(socket_path) {
        Ok(_) => RunningStatus::Running,
        Err(err) => classify_io_error(span, &socket_path.to_string_lossy(), &err),
    }
}

fn check_tcp_port(span: &Span, host: &str, port: u16) -> RunningStatus {
    match TcpStream::connect((host, port)) {
        Ok(_) => RunningStatus::Running,
        Err(err) => classify_io_error(span, &format!("{host}:{port}"), &err),
    }
}

fn check_heartbeat(span: &Span, path: &Path, max_age: Duration) -> RunningStatus {
    let reader = LocalHeartbeatReader::new(path, Arc::new(UnixFileReader::new()));
    check_heartbeat_with(
        span,
        &path.to_string_lossy(),
        max_age,
        &Unix::new(),
        &reader,
    )
}

fn check_heartbeat_with(
    span: &Span,
    target: &str,
    max_age: Duration,
    os: &dyn Os,
    reader: &dyn HeartbeatReader,
) -> RunningStatus {
    let heartbeat = match reader.read() {
        Ok(heartbeat) => heartbeat,
        Err(HeartbeatReaderError::FileSystemError(err)) => {
            return classify_file_error(span, target, &err);
        }
        Err(err) => return unknown_heartbeat(span, target, &err),
    };

    match heartbeat.age() {
        Some(age) if age <= max_age => check_heartbeat_pid(span, target, heartbeat.pid, os),
        Some(_) => RunningStatus::NotRunning,
        None => RunningStatus::Unknown,
    }
}

fn check_heartbeat_pid(span: &Span, target: &str, pid: u32, os: &dyn Os) -> RunningStatus {
    match os.is_active_pid(pid) {
        Ok(true) => RunningStatus::Running,
        Ok(false) => RunningStatus::NotRunning,
        Err(err) => unknown_heartbeat(span, target, &err),
    }
}

fn classify_file_error(span: &Span, target: &str, err: &FileSystemError) -> RunningStatus {
    match err {
        FileSystemError::NotFoundError(_) => RunningStatus::NotRunning,
        FileSystemError::IoError(error) | FileSystemError::IoErrorAtPath { error, .. } => {
            classify_io_error(span, target, error)
        }
        _ => unknown_heartbeat(span, target, err),
    }
}

fn unknown_heartbeat(span: &Span, target: &str, err: &dyn std::fmt::Display) -> RunningStatus {
    span.message(
        Level::Warn,
        &format!("Could not determine status of '{target}': '{err}'"),
    );
    RunningStatus::Unknown
}

fn classify_io_error(span: &Span, target: &str, err: &std::io::Error) -> RunningStatus {
    let status = running_status_for_error_kind(err.kind());
    if status == RunningStatus::Unknown {
        span.message(
            Level::Warn,
            &format!("Could not determine status of '{target}': '{err}'"),
        );
    }
    status
}

fn running_status_for_error_kind(kind: ErrorKind) -> RunningStatus {
    match kind {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::PermissionDenied => {
            RunningStatus::NotRunning
        }
        _ => RunningStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ListenerDefinition, LivenessCheck, SocketListenerFactory, check_heartbeat_with,
        check_liveness, running_status_for_error_kind,
    };
    use crate::RunningStatus;
    use file_system::{
        FileSystemError, MockBindableUnixDomainSocketFile, MockFileDeleter, MockListener,
        MockPermissions, Modes,
    };
    use heartbeat::{Heartbeat, HeartbeatReaderError, MockHeartbeatReader};
    use log::{Event, Reporter, ScopeKind, Span};
    use os::MockOs;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn span() -> Span {
        Span::new(Arc::new(NullReporter), "test", ScopeKind::Group)
    }

    #[test]
    fn test_should_delete_bind_then_enforce_ownership_and_mode() {
        let mut file_deleter = MockFileDeleter::new();
        let mut permissions = MockPermissions::new();
        let mut socket_file = MockBindableUnixDomainSocketFile::new();

        file_deleter.expect_file_to_be_deleted("/run/douglas/test.sock");
        socket_file.expect_bind_with("/run/douglas/test.sock", || Box::new(MockListener::new()));

        permissions.expect_ownership_and_mode_to_be_set(
            "/run/douglas/test.sock",
            "root",
            "douglas-admin",
            Modes::OwnerReadWriteGroupReadWrite,
        );

        let factory = SocketListenerFactory::new(
            ListenerDefinition::new(
                Path::new("/run/douglas/test.sock"),
                "root",
                "douglas-admin",
                Modes::OwnerReadWriteGroupReadWrite,
            ),
            Arc::new(file_deleter),
            Arc::new(permissions),
            Arc::new(socket_file),
        );

        let result = factory.create(&span());

        assert!(result.is_ok());
    }

    #[test]
    fn test_should_report_not_running_when_socket_file_missing() {
        let status = check_liveness(
            &span(),
            &LivenessCheck::UnixSocket(PathBuf::from("/run/douglas/definitely-missing.sock")),
        );

        assert!(status == RunningStatus::NotRunning);
    }

    const HEARTBEAT_PATH: &str = "/run/douglas/woodward-heartbeat/heartbeat";

    fn recent() -> SystemTime {
        SystemTime::now() - Duration::from_secs(1)
    }

    fn reader_returning(pid: u32, written_at: SystemTime) -> MockHeartbeatReader {
        let mut reader = MockHeartbeatReader::new();
        reader
            .expect_read()
            .returning(move || Ok(Heartbeat { pid, written_at }));
        reader
    }

    fn reader_failing_with(error: fn() -> HeartbeatReaderError) -> MockHeartbeatReader {
        let mut reader = MockHeartbeatReader::new();
        reader.expect_read().returning(move || Err(error()));
        reader
    }

    fn not_found() -> HeartbeatReaderError {
        HeartbeatReaderError::FileSystemError(FileSystemError::NotFoundError(PathBuf::from(
            HEARTBEAT_PATH,
        )))
    }

    fn timed_out() -> HeartbeatReaderError {
        HeartbeatReaderError::FileSystemError(FileSystemError::IoErrorAtPath {
            path: PathBuf::from(HEARTBEAT_PATH),
            error: std::io::Error::from(ErrorKind::TimedOut),
        })
    }

    fn unparseable() -> HeartbeatReaderError {
        HeartbeatReaderError::SupportFileSerializationError(
            serde_json::from_str::<()>("not json").unwrap_err(),
        )
    }

    fn check_heartbeat(os: &MockOs, reader: &MockHeartbeatReader) -> RunningStatus {
        check_heartbeat_with(&span(), HEARTBEAT_PATH, Duration::from_secs(15), os, reader)
    }

    #[test]
    fn test_should_report_running_when_heartbeat_is_recent_and_its_pid_is_active() {
        let mut os = MockOs::new();
        os.given_pid_is_active(4242);
        let reader = reader_returning(4242, recent());

        let status = check_heartbeat(&os, &reader);

        assert!(status == RunningStatus::Running);
    }

    #[test]
    fn test_should_report_not_running_when_heartbeat_is_recent_but_its_pid_is_gone() {
        let mut os = MockOs::new();
        os.given_pid_is_not_active(4242);
        let reader = reader_returning(4242, recent());

        let status = check_heartbeat(&os, &reader);

        assert!(status == RunningStatus::NotRunning);
    }

    #[test]
    fn test_should_report_not_running_without_checking_the_pid_when_stale() {
        let os = MockOs::new();
        let reader = reader_returning(4242, SystemTime::now() - Duration::from_secs(60));

        let status = check_heartbeat(&os, &reader);

        assert!(status == RunningStatus::NotRunning);
    }

    #[test]
    fn test_should_report_unknown_when_written_at_is_in_the_future() {
        let os = MockOs::new();
        let reader = reader_returning(4242, SystemTime::now() + Duration::from_secs(600));

        let status = check_heartbeat(&os, &reader);

        assert!(status == RunningStatus::Unknown);
    }

    #[test]
    fn test_should_report_unknown_when_the_pid_cannot_be_checked() {
        let mut os = MockOs::new();
        os.expect_is_active_pid()
            .returning(|_| Err(os::OsError::PidTooLarge));
        let reader = reader_returning(4242, recent());

        let status = check_heartbeat(&os, &reader);

        assert!(status == RunningStatus::Unknown);
    }

    #[test]
    fn test_should_report_not_running_when_the_heartbeat_file_is_missing() {
        let os = MockOs::new();
        let reader = reader_failing_with(not_found);

        let status = check_heartbeat(&os, &reader);

        assert!(status == RunningStatus::NotRunning);
    }

    #[test]
    fn test_should_report_unknown_when_the_heartbeat_cannot_be_read() {
        let os = MockOs::new();
        let reader = reader_failing_with(timed_out);

        let status = check_heartbeat(&os, &reader);

        assert!(status == RunningStatus::Unknown);
    }

    #[test]
    fn test_should_report_unknown_when_the_heartbeat_cannot_be_parsed() {
        let os = MockOs::new();
        let reader = reader_failing_with(unparseable);

        let status = check_heartbeat(&os, &reader);

        assert!(status == RunningStatus::Unknown);
    }

    #[test]
    fn test_should_classify_connection_refused_and_missing_as_not_running() {
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::ConnectionRefused,
            ErrorKind::PermissionDenied,
        ] {
            assert!(running_status_for_error_kind(kind) == RunningStatus::NotRunning);
        }
    }

    #[test]
    fn test_should_classify_other_errors_as_unknown() {
        assert!(running_status_for_error_kind(ErrorKind::TimedOut) == RunningStatus::Unknown);
    }
}
