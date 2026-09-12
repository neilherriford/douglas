use crate::RunningStatus;
use file_system::{
    BindableUnixDomainSocketFile, FileDeleter, FileSystemError, Listener, Modes, Permissions,
};
use log::{Level, Outcome, ScopeKind, Span};
use std::io::ErrorKind;
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

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
        Err(err) => classify_io_error(span, &socket_path.to_string_lossy(), err),
    }
}

fn check_tcp_port(span: &Span, host: &str, port: u16) -> RunningStatus {
    match TcpStream::connect((host, port)) {
        Ok(_) => RunningStatus::Running,
        Err(err) => classify_io_error(span, &format!("{host}:{port}"), err),
    }
}

fn check_heartbeat(span: &Span, path: &Path, max_age: Duration) -> RunningStatus {
    let modified = match std::fs::metadata(path).and_then(|metadata| metadata.modified()) {
        Ok(modified) => modified,
        Err(err) => return classify_io_error(span, &path.to_string_lossy(), err),
    };

    match SystemTime::now().duration_since(modified) {
        Ok(elapsed) if elapsed <= max_age => RunningStatus::Running,
        Ok(_) => RunningStatus::NotRunning,
        Err(_) => RunningStatus::Unknown,
    }
}

fn classify_io_error(span: &Span, target: &str, err: std::io::Error) -> RunningStatus {
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
        ListenerDefinition, LivenessCheck, SocketListenerFactory, check_liveness,
        running_status_for_error_kind,
    };
    use crate::RunningStatus;
    use file_system::{
        MockBindableUnixDomainSocketFile, MockFileDeleter, MockListener, MockPermissions, Modes,
    };
    use log::{Event, Reporter, ScopeKind, Span};
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

    #[test]
    fn test_should_report_running_when_heartbeat_is_recent() {
        let mut path = std::env::temp_dir();
        path.push(format!("blueprint-heartbeat-test-{}-recent", std::process::id()));
        std::fs::write(&path, "").expect("should write heartbeat file");

        let status = check_liveness(
            &span(),
            &LivenessCheck::Heartbeat {
                path: path.clone(),
                max_age: Duration::from_secs(15),
            },
        );

        let _ = std::fs::remove_file(&path);
        assert!(status == RunningStatus::Running);
    }

    #[test]
    fn test_should_report_not_running_when_heartbeat_is_stale() {
        let mut path = std::env::temp_dir();
        path.push(format!("blueprint-heartbeat-test-{}-stale", std::process::id()));
        std::fs::write(&path, "").expect("should write heartbeat file");
        let file = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("should open heartbeat file");
        file.set_modified(SystemTime::now() - Duration::from_secs(60))
            .expect("should backdate mtime");

        let status = check_liveness(
            &span(),
            &LivenessCheck::Heartbeat {
                path: path.clone(),
                max_age: Duration::from_secs(15),
            },
        );

        let _ = std::fs::remove_file(&path);
        assert!(status == RunningStatus::NotRunning);
    }

    #[test]
    fn test_should_report_not_running_when_heartbeat_file_is_missing() {
        let status = check_liveness(
            &span(),
            &LivenessCheck::Heartbeat {
                path: PathBuf::from("/run/douglas/definitely-missing-heartbeat"),
                max_age: Duration::from_secs(15),
            },
        );

        assert!(status == RunningStatus::NotRunning);
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
