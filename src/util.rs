use blueprint::{
    RunningStatus,
    listener::{LivenessCheck, check_liveness},
};
use command_fds::{CommandFdExt, FdMapping};
use file_system::{FileReader, FileSystemError};
use log::{Level, Outcome, ScopeGuard, Span};
use os::Os;
use os_pipe::{PipeReader, PipeWriter};
use std::{
    os::fd::{AsRawFd, OwnedFd},
    time::{Duration, Instant},
};

pub(crate) fn join_display<T: std::fmt::Display>(items: &[T], separator: &str) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

pub(crate) fn read_if_present(
    file_reader: &dyn FileReader,
    path: &std::path::Path,
) -> Result<Option<String>, FileSystemError> {
    match file_reader.read_all(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(err) if err.is_not_found() => Ok(None),
        Err(err) => Err(err),
    }
}

pub(crate) fn spawn_service(
    name: &'static str,
    pipe: Option<(PipeReader, PipeWriter)>,
    needs_notify_fd: bool,
    os: &dyn Os,
    span: &Span,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut command = std::process::Command::new(os.current_executable()?);
    command.args(["service", name]);

    if !needs_notify_fd {
        command.spawn()?;
        return Ok(());
    }

    let (pipe_reader, pipe_writer) = match pipe {
        Some(pipe) => pipe,
        None => os_pipe::pipe()?,
    };

    let fd = pipe_writer.as_raw_fd();
    command.args(["--notify-fd", &fd.to_string()]);
    match command.fd_mappings(vec![FdMapping {
        parent_fd: OwnedFd::from(pipe_writer),
        child_fd: fd,
    }]) {
        Ok(cmd) => {
            cmd.spawn()?;
        }
        Err(err) => return Err(Box::new(err)),
    }
    forward_logs_in_background(name, pipe_reader, span.clone());

    Ok(())
}

fn forward_logs_in_background(service_name: &'static str, pipe_reader: PipeReader, span: Span) {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(pipe_reader);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let Ok(mut event) = serde_json::from_str::<log::Event>(&line) else {
                continue;
            };
            tag_event_with_service(&mut event, service_name);
            span.reporter.emit(event);
        }
    });
}

fn tag_event_with_service(event: &mut log::Event, service_name: &str) {
    match &mut event.kind {
        log::EventKind::ScopeStarted { label, .. } | log::EventKind::ScopeEnded { label, .. } => {
            *label = format!("[{service_name}] {label}");
        }
        log::EventKind::Message { text, .. } => {
            *text = format!("[{service_name}] {text}");
        }
        log::EventKind::PlanHint { .. } | log::EventKind::Progress { .. } => {}
    }
}

pub(crate) fn require<T, E: std::fmt::Display>(
    guard: &ScopeGuard,
    step: &str,
    result: Result<T, E>,
) -> Option<T> {
    result
        .inspect_err(|err| {
            guard.span().message(Level::Warn, &format!("{step}: {err}"));
            guard.finish_with_outcome(Outcome::Failed);
        })
        .ok()
}

pub(crate) fn conclude(guard: &ScopeGuard, succeeded: bool) -> bool {
    guard.finish_with_outcome(if succeeded {
        Outcome::Ok
    } else {
        Outcome::Failed
    });
    succeeded
}

pub(crate) async fn wait_until_running(liveness: &LivenessCheck, span: &Span) -> bool {
    let deadline = Instant::now() + Duration::from_mins(5);
    loop {
        if check_liveness(span, liveness) == RunningStatus::Running {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    struct CapturingReporter {
        outcomes: std::sync::Mutex<Vec<Outcome>>,
    }

    impl log::Reporter for CapturingReporter {
        fn emit(&self, event: log::Event) {
            if let log::EventKind::ScopeEnded { outcome, .. } = event.kind {
                let Ok(mut outcomes) = self.outcomes.lock() else {
                    panic!("outcomes mutex poisoned");
                };
                outcomes.push(outcome);
            }
        }
    }

    fn conclude_with(succeeded: bool) -> (bool, Vec<Outcome>) {
        let reporter = std::sync::Arc::new(CapturingReporter {
            outcomes: std::sync::Mutex::new(Vec::new()),
        });
        let guard = Span::new(
            std::sync::Arc::clone(&reporter) as std::sync::Arc<dyn log::Reporter>,
            "test",
            log::ScopeKind::Group,
        )
        .start_guard();

        let result = conclude(&guard, succeeded);

        let Ok(outcomes) = reporter.outcomes.lock() else {
            panic!("outcomes mutex poisoned");
        };
        (result, outcomes.clone())
    }

    #[test]
    fn test_conclude_should_finish_ok_and_report_success() {
        let (result, outcomes) = conclude_with(true);

        assert!(result);
        assert!(matches!(outcomes.as_slice(), [Outcome::Ok]));
    }

    #[test]
    fn test_conclude_should_finish_failed_and_report_failure() {
        let (result, outcomes) = conclude_with(false);

        assert!(!result);
        assert!(matches!(outcomes.as_slice(), [Outcome::Failed]));
    }

    #[test]
    fn test_join_display_should_join_items_with_the_separator() {
        assert_eq!(join_display(&[1, 2, 3], "; "), "1; 2; 3");
    }

    #[test]
    fn test_join_display_should_be_empty_for_no_items() {
        assert_eq!(join_display::<u8>(&[], ", "), "");
    }

    #[test]
    fn test_join_display_should_not_add_a_separator_around_a_single_item() {
        assert_eq!(join_display(&["only"], "; "), "only");
    }

    #[test]
    fn test_read_if_present_should_return_the_contents() {
        let mut reader = file_system::MockFileReader::new();
        reader
            .expect_read_all()
            .returning(|_| Ok("contents".to_string()));

        let result = read_if_present(&reader, std::path::Path::new("/x"));

        assert!(matches!(result, Ok(Some(contents)) if contents == "contents"));
    }

    #[test]
    fn test_read_if_present_should_return_none_when_the_file_is_missing() {
        let mut reader = file_system::MockFileReader::new();
        reader
            .expect_read_all()
            .returning(|path| Err(FileSystemError::NotFoundError(path.to_path_buf())));

        let result = read_if_present(&reader, std::path::Path::new("/x"));

        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_read_if_present_should_pass_on_any_other_error() {
        let mut reader = file_system::MockFileReader::new();
        reader.expect_read_all().returning(|path| {
            Err(FileSystemError::IoErrorAtPath {
                path: path.to_path_buf(),
                error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            })
        });

        let result = read_if_present(&reader, std::path::Path::new("/x"));

        assert!(result.is_err());
    }
    use super::*;
    use log::{Event, EventKind, Level, ScopeId, ScopeKind};
    use std::sync::Arc;

    struct NullReporter;
    impl log::Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn span() -> Span {
        Span::new(Arc::new(NullReporter), "test", ScopeKind::Group)
    }

    #[test]
    fn test_tag_event_with_service_should_prefix_scope_started_label() {
        let mut event = Event::start_scope(ScopeId::new(), "Bootstrapping", ScopeKind::Group);

        tag_event_with_service(&mut event, "seedbank");

        assert!(matches!(
            event.kind,
            EventKind::ScopeStarted { ref label, .. } if label == "[seedbank] Bootstrapping"
        ));
    }

    #[test]
    fn test_tag_event_with_service_should_prefix_message_text() {
        let mut event = Event::new(
            ScopeId::new(),
            EventKind::Message {
                level: Level::Info,
                text: "hello".to_string(),
            },
        );

        tag_event_with_service(&mut event, "resin");

        assert!(matches!(
            event.kind,
            EventKind::Message { ref text, .. } if text == "[resin] hello"
        ));
    }

    #[test]
    fn test_tag_event_with_service_should_leave_plan_hint_untouched() {
        let mut event = Event::new(
            ScopeId::new(),
            EventKind::PlanHint {
                steps: vec!["one".to_string()],
            },
        );

        tag_event_with_service(&mut event, "resin");

        assert!(matches!(
            event.kind,
            EventKind::PlanHint { ref steps } if steps == &vec!["one".to_string()]
        ));
    }

    #[tokio::test]
    async fn test_wait_until_running_should_return_immediately_when_already_running() {
        let Ok(listener) = std::net::TcpListener::bind("127.0.0.1:0") else {
            panic!("should bind");
        };
        let Ok(local_addr) = listener.local_addr() else {
            panic!("should have local addr");
        };
        let port = local_addr.port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });

        let liveness = LivenessCheck::TcpPort {
            host: "127.0.0.1".to_string(),
            port,
        };

        let result = wait_until_running(&liveness, &span()).await;

        assert!(result);
    }
}
