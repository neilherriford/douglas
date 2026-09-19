use config::DouglasFolders;
use file_system::{FileDeleter, FileReader, FileSystemError, FileWriter, path_to_string};
use heartbeat::{
    HeartbeatReader, HeartbeatReaderError, HeartbeatWriter, LocalHeartbeatReader,
    LocalHeartbeatWriter,
};
use log::{Reporter, ScopeKind, Span};
use os::Os;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};
use thiserror::Error;
use tokio::sync::broadcast::{self, Sender};

pub use bootstrap::{DOUGLAS_WOODWARD_GROUP, DOUGLAS_WOODWARD_USER, service_definition};

mod bootstrap;

pub use config::services::WOODWARD;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct SupervisionFailure {
    pub service_name: String,
    pub gave_up_at: std::time::SystemTime,
    pub restart_count: u8,
    pub kick_failures: u8,
}

pub fn read_supervision_failure(
    file_reader: &dyn FileReader,
    path: &Path,
) -> Result<Option<SupervisionFailure>, HeartbeatReaderError> {
    match file_reader.read_all(path) {
        Ok(raw) => Ok(Some(serde_json::from_str::<SupervisionFailure>(&raw)?)),
        Err(FileSystemError::NotFoundError(_)) => Ok(None),
        Err(FileSystemError::IoErrorAtPath { error, .. })
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(err) => Err(err.into()),
    }
}

/*
 * Simplified state diagram
 *
 *               ↓
 *             ┏━━━━┓
 * ╭──────────→┃wait┃←──────────────────────────────────────────────────────────────────╮
 * │           ┗━━━━┛                                                                   │
 * │              ↓                      ┏━━━━━━━━━━━━━━━━━━━┓                          │
 * │     last_restart > 300s ago?─(YES)─→┃Clear restart_count┃                          │
 * │              │                      ┗━━━━━━━━━━━━━━━━━━━┛                          │
 * │             (NO)                              │                                    │
 * │              ├────────────────────────────────╯                                    │
 * │              ↓                      ┏━━━━━━━━━━━━━━━━━━━┓                          │
 * │      last_kick > 300s ago?──(YES)──→┃Clear kick_failures┃                          │
 * │              │                      ┗━━━━━━━━━━━━━━━━━━━┛                          │
 * │             (NO)                            │                                      │
 * │              ├──────────────────────────────╯                                      │
 * │              ↓                   ┏━━━━━━━━━━━━━━┓                                  │
 * │      kick_failures ≥ 3  ┬─(YES)─→┃log 'gave up' ┃──────────────────────────────────┤
 * │    OR restart_count > 3?╯        ┗━━━━━━━━━━━━━━┛                                  │
 * │              │                                                                     │
 * │             (NO)                                                                   │
 * │              │                 ┏━━━━━━━━━━━━━━━━━━━━━━━┓                           │
 * │              ↓                 ┃Clear failure_count    ┃                           │
 * │  heartbeat_age ≤ 15s?───(YES)─→┃Clear kick_failures    ┃───────────────────────────┤
 * │              │                 ┃Clear recovering_since ┃                           │
 * │             (NO)               ┃Log 'ok'               ┃                           │
 * │              │                 ┗━━━━━━━━━━━━━━━━━━━━━━━┛                           │
 * │              ↓                        ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━┓                │
 * │  recovering_since set and             ┃Log 'missed heartbeat'     ┃                │
 * │  < 90s ago?─────────────────(YES)────→┃(don't count it — it just  ┃────────────────┤
 * │  (just started, or just               ┃ started/got kicked, give  ┃                │
 * │   kicked — give it grace)             ┃ it time to warm up)       ┃                │
 * │              │                        ┗━━━━━━━━━━━━━━━━━━━━━━━━━━━┛                │
 * │             (NO)                                                                   │
 * │              ↓                                                                     │
 * │  ┏━━━━━━━━━━━━━━━━━━━━━━━┓                                                         │
 * │  ┃increment failure_count┃                                                         │
 * │  ┗━━━━━━━━━━━━━━━━━━━━━━━┛        ┏━━━━━━━━━━━━━┓                                  │
 * │              ↓                    ┃Log 'kicking'┃                                  │
 * │      failure_count > 3?────(YES)─→┃kick service ┃                                  │
 * │              │                    ┗━━━━━━━━━━━━━┛        ┏━━━━━━━━━━━━━━━━━━━━━━━┓ │
 * │             (NO)                         ↓               ┃Reset failure_count    ┃ │
 * │              │                    kick succeeded?─(YES)─→┃Reset kick_failures    ┃ │
 * │              ↓                           │               ┃Increment restart_count┃ │
 * │    ┏━━━━━━━━━━━━━━━━━━━━━━┓             (NO)             ┃Set last_restart to now┃ │
 * │    ┃Log 'missed heartbeat'┃              │               ┃Set last_kick to now   ┃ │
 * │    ┗━━━━━━━━━━━━━━━━━━━━━━┛              ↓               ┃Set recovering_since   ┃ │
 * ╰──────────────╯                ┏━━━━━━━━━━━━━━━━━━━━━━━┓  ┃  to now               ┃ │
 *                                 ┃increment kick_failures┃  ┗━━━━━━━━━━━━━━━━━━━━━━━┛ │
 *                                 ┃Set last_kick to now   ┃             ↓              │
 *                                 ┗━━━━━━━━━━━━━━━━━━━━━━━┛             │              │
 *                                            │                          │              │
 *                                            ╰──────────────────────────┴──────────────╯
 *
 * recovering_since is set fresh when a tally is created (so a cold-started
 * service gets the same grace as a kicked one) and cleared the moment a
 * timely heartbeat is seen. It tracks wall-clock time since we started
 * waiting on THIS attempt — not the heartbeat file's own mtime — precisely
 * so a service that has never yet heartbeated this attempt (file missing,
 * or still carrying a stale timestamp from before a freeze/crash/kick)
 * still gets the benefit of the doubt while it's still within the 90s
 * startup grace, and is caught fast (the old ~15-20s detection) the moment
 * that grace runs out or a heartbeat was already confirmed since. Gating on
 * the heartbeat file's age directly doesn't work: a freshly kicked service
 * can't be told apart from a genuinely-stuck one by file mtime alone, since
 * both look identically stale until the new process's first heartbeat
 * actually lands.
 */

#[derive(Error, Debug)]
pub enum Error {
    #[error("Cannot be root")]
    CannotBeRoot,
    #[error("Os error: {0}")]
    OsError(#[from] os::OsError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

const MAX_ELAPSED_SECONDS: u64 = 15;
const STARTUP_GRACE_SECONDS: u64 = 90;
const MAX_MISSED_HEARTBEATS: u8 = 3;
const MAX_FAILED_RESTARTS: u8 = 3;
const MAX_KICK_FAILURES: u8 = 3;
const RESTART_WINDOW_SECONDS: u64 = 300;

enum ServiceCheckResult {
    Okay,
    NeedsKick,
    MissedHeartbeat,
}

enum SupervisionDecision {
    Check,
    GivenUp,
}

struct ServiceTally {
    failure_count: u8,
    restart_count: u8,
    consecutive_kick_failures: u8,
    last_restart_at: Option<std::time::SystemTime>,
    last_kick_at: Option<std::time::SystemTime>,
    recovering_since: Option<std::time::SystemTime>,
    failure_marked: bool,
}

impl ServiceTally {
    fn new() -> Self {
        Self {
            failure_count: 0,
            restart_count: 0,
            consecutive_kick_failures: 0,
            last_restart_at: None,
            last_kick_at: None,
            recovering_since: Some(std::time::SystemTime::now()),
            failure_marked: false,
        }
    }

    fn decide(&mut self) -> SupervisionDecision {
        if let Some(last_restart) = self.last_restart_at
            && elapsed_exceeds(last_restart, RESTART_WINDOW_SECONDS)
        {
            self.restart_count = 0;
            self.last_restart_at = None;
        }

        if let Some(last_kick) = self.last_kick_at
            && elapsed_exceeds(last_kick, RESTART_WINDOW_SECONDS)
        {
            self.consecutive_kick_failures = 0;
            self.last_kick_at = None;
        }

        if self.consecutive_kick_failures >= MAX_KICK_FAILURES
            || self.restart_count > MAX_FAILED_RESTARTS
        {
            return SupervisionDecision::GivenUp;
        }

        SupervisionDecision::Check
    }

    fn record_heartbeat(&mut self) -> ServiceCheckResult {
        self.failure_count = 0;
        self.consecutive_kick_failures = 0;
        self.recovering_since = None;
        ServiceCheckResult::Okay
    }

    fn record_missed_heartbeat(&mut self) -> ServiceCheckResult {
        if let Some(recovering_since) = self.recovering_since
            && !elapsed_exceeds(recovering_since, STARTUP_GRACE_SECONDS)
        {
            return ServiceCheckResult::MissedHeartbeat;
        }

        self.failure_count += 1;

        if self.failure_count > MAX_MISSED_HEARTBEATS {
            ServiceCheckResult::NeedsKick
        } else {
            ServiceCheckResult::MissedHeartbeat
        }
    }

    fn record_kick_success(&mut self) {
        let now = std::time::SystemTime::now();
        self.failure_count = 0;
        self.consecutive_kick_failures = 0;
        self.restart_count += 1;
        self.last_restart_at = Some(now);
        self.last_kick_at = Some(now);
        self.recovering_since = Some(now);
    }

    fn record_kick_failure(&mut self) {
        self.consecutive_kick_failures += 1;
        self.last_kick_at = Some(std::time::SystemTime::now());
    }
}

fn elapsed_exceeds(since: std::time::SystemTime, seconds: u64) -> bool {
    std::time::SystemTime::now()
        .duration_since(since)
        .is_ok_and(|elapsed| elapsed.as_secs() > seconds)
}

fn heartbeat_age(reader: &dyn HeartbeatReader) -> std::time::Duration {
    reader
        .read()
        .ok()
        .and_then(|heartbeat| {
            std::time::SystemTime::now()
                .duration_since(heartbeat.written_at)
                .ok()
        })
        .unwrap_or(std::time::Duration::MAX)
}

fn kick(os: &dyn Os, binary_link: &std::path::Path, service_name: &str) -> Result<(), Error> {
    let args = vec![
        "-n".to_string(),
        path_to_string(binary_link),
        "kick".to_string(),
        service_name.to_string(),
    ];

    os.execute("sudo", args, Vec::new())?;
    Ok(())
}

struct ServiceCheck {
    service_name: String,
    binary_link: PathBuf,
    failure_marker: PathBuf,
    reader: Box<dyn HeartbeatReader>,
    os: Arc<dyn Os>,
    file_writer: Arc<dyn FileWriter>,
    file_deleter: Arc<dyn FileDeleter>,
    tally: Mutex<ServiceTally>,
}

impl ServiceCheck {
    pub fn new(
        service_name: &str,
        douglas_folders: &DouglasFolders,
        file_reader: Arc<dyn FileReader>,
        file_writer: Arc<dyn FileWriter>,
        file_deleter: Arc<dyn FileDeleter>,
        os: Arc<dyn Os>,
    ) -> Self {
        let heartbeat_file = douglas_folders.service_heartbeat_file(service_name);
        let reader = LocalHeartbeatReader::new(&heartbeat_file, file_reader);

        Self {
            service_name: service_name.to_string(),
            binary_link: douglas_folders.binary_link(),
            failure_marker: douglas_folders.supervisor_failure_marker(service_name),
            reader: Box::new(reader),
            os,
            file_writer,
            file_deleter,
            tally: Mutex::new(ServiceTally::new()),
        }
    }

    fn tally(&self) -> MutexGuard<'_, ServiceTally> {
        self.tally.lock().expect("woodward status lock poisoned")
    }

    async fn mark_failed(
        &self,
        failure: &SupervisionFailure,
    ) -> Result<(), file_system::FileSystemError> {
        let serialized = serde_json::to_string(failure)
            .map_err(|err| file_system::FileSystemError::IoError(std::io::Error::other(err)))?;

        let file_writer = Arc::clone(&self.file_writer);
        let failure_marker = self.failure_marker.clone();
        tokio::task::spawn_blocking(move || file_writer.write_all(&failure_marker, &serialized))
            .await
            .unwrap_or_else(|join_err| {
                Err(file_system::FileSystemError::IoError(
                    std::io::Error::other(join_err),
                ))
            })
    }

    async fn clear_failure(&self) -> Result<(), file_system::FileSystemError> {
        let file_deleter = Arc::clone(&self.file_deleter);
        let failure_marker = self.failure_marker.clone();
        tokio::task::spawn_blocking(move || file_deleter.delete(&failure_marker))
            .await
            .unwrap_or_else(|join_err| {
                Err(file_system::FileSystemError::IoError(
                    std::io::Error::other(join_err),
                ))
            })
    }
}

pub struct Server {
    reporter: Arc<dyn Reporter>,
    shutdown_sender: Sender<()>,
    service_checks: Vec<ServiceCheck>,
    heartbeat_writer: Box<dyn HeartbeatWriter>,
}

impl Server {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        file_reader: Arc<dyn FileReader>,
        file_writer: Arc<dyn FileWriter>,
        file_deleter: Arc<dyn FileDeleter>,
        os: Arc<dyn Os>,
        douglas_folders: DouglasFolders,
    ) -> Self {
        let (shutdown_sender, _) = broadcast::channel::<()>(1);
        let service_checks = vec![
            ServiceCheck::new(
                config::services::BRACT,
                &douglas_folders,
                Arc::clone(&file_reader),
                Arc::clone(&file_writer),
                Arc::clone(&file_deleter),
                Arc::clone(&os),
            ),
            ServiceCheck::new(
                config::services::RESIN,
                &douglas_folders,
                Arc::clone(&file_reader),
                Arc::clone(&file_writer),
                Arc::clone(&file_deleter),
                Arc::clone(&os),
            ),
            ServiceCheck::new(
                config::services::SEEDBANK,
                &douglas_folders,
                Arc::clone(&file_reader),
                Arc::clone(&file_writer),
                Arc::clone(&file_deleter),
                Arc::clone(&os),
            ),
        ];
        let heartbeat_writer = Box::new(LocalHeartbeatWriter::new(
            file_writer,
            &douglas_folders.service_heartbeat_file(config::services::WOODWARD),
        ));

        Self {
            reporter,
            shutdown_sender,
            service_checks,
            heartbeat_writer,
        }
    }

    pub async fn start(self: Arc<Self>) -> Result<(), Error> {
        let span = Span::new(
            Arc::clone(&self.reporter),
            "Starting woodward",
            ScopeKind::Group,
        );
        let mut shutdown = self.shutdown_sender.subscribe();

        let accept_loops = async {
            let check_task = {
                let server = Arc::clone(&self);
                tokio::spawn(async move { Self::check_task(server).await })
            };
            let heartbeat_task = {
                let server = Arc::clone(&self);
                tokio::spawn(async move { Self::heartbeat_loop(server).await })
            };

            check_task.await.map_err(std::io::Error::other)?;
            heartbeat_task.await.map_err(std::io::Error::other)?;
            Ok::<_, Error>(())
        };

        tokio::select! {
            r = accept_loops => r?,
            _ = shutdown.recv() => {},
        }

        span.create_scoped_reporter().finish(log::Outcome::Ok);
        Ok(())
    }

    async fn heartbeat_loop(server: Arc<Self>) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;

            let span = Span::new(
                Arc::clone(&server.reporter),
                "Updating heartbeat",
                ScopeKind::Task,
            );

            if let Err(err) = server.heartbeat_writer.write() {
                span.message(log::Level::Warn, &format!("Heartbeat write failed: {err}"));
            }
        }
    }

    async fn check_task(server: Arc<Self>) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));

        loop {
            interval.tick().await;

            let span = Span::new(
                Arc::clone(&server.reporter),
                "Checking core services",
                ScopeKind::Task,
            );

            for service_check in &server.service_checks {
                check_one_service(&span, service_check).await;
            }
        }
    }
}

async fn check_one_service(span: &Span, service_check: &ServiceCheck) {
    let decision = service_check.tally().decide();
    match decision {
        SupervisionDecision::GivenUp => {
            note_given_up(span, service_check).await;
            return;
        }
        SupervisionDecision::Check => {}
    }

    let age = heartbeat_age(service_check.reader.as_ref());
    let result = if age <= std::time::Duration::from_secs(MAX_ELAPSED_SECONDS) {
        service_check.tally().record_heartbeat()
    } else {
        service_check.tally().record_missed_heartbeat()
    };

    match result {
        ServiceCheckResult::Okay => {
            note_recovered(span, service_check).await;
            span.message(
                log::Level::Info,
                &format!(
                    "Received timely heartbeat from {}",
                    service_check.service_name
                ),
            );
        }
        ServiceCheckResult::MissedHeartbeat => {
            span.message(
                log::Level::Warn,
                &format!("Service {} missed a heartbeat", service_check.service_name),
            );
        }
        ServiceCheckResult::NeedsKick => {
            span.message(
                log::Level::Warn,
                &format!("Kicking service {}…", service_check.service_name),
            );

            kick_service(span, service_check).await;
        }
    }
}

async fn note_given_up(span: &Span, service_check: &ServiceCheck) {
    if service_check.tally().failure_marked {
        return;
    }

    let failure = {
        let tally = service_check.tally();
        SupervisionFailure {
            service_name: service_check.service_name.clone(),
            gave_up_at: std::time::SystemTime::now(),
            restart_count: tally.restart_count,
            kick_failures: tally.consecutive_kick_failures,
        }
    };

    match service_check.mark_failed(&failure).await {
        Ok(()) => {
            service_check.tally().failure_marked = true;
            span.message(
                log::Level::Warn,
                &format!(
                    "Gave up supervising {} after {} restarts and {} failed kicks",
                    service_check.service_name, failure.restart_count, failure.kick_failures
                ),
            );
        }
        Err(err) => span.message(
            log::Level::Warn,
            &format!(
                "Gave up supervising {} but could not write the failure marker: {err}",
                service_check.service_name
            ),
        ),
    }
}

async fn note_recovered(span: &Span, service_check: &ServiceCheck) {
    if !service_check.tally().failure_marked {
        return;
    }

    match service_check.clear_failure().await {
        Ok(()) => {
            service_check.tally().failure_marked = false;
            span.message(
                log::Level::Info,
                &format!("Resumed supervising {}", service_check.service_name),
            );
        }
        Err(err) => span.message(
            log::Level::Warn,
            &format!(
                "{} recovered but its failure marker could not be cleared: {err}",
                service_check.service_name
            ),
        ),
    }
}

async fn kick_service(span: &Span, service_check: &ServiceCheck) {
    let os = Arc::clone(&service_check.os);
    let binary_link = service_check.binary_link.clone();
    let name = service_check.service_name.clone();

    match tokio::task::spawn_blocking(move || kick(os.as_ref(), &binary_link, &name)).await {
        Ok(Ok(())) => {
            service_check.tally().record_kick_success();
            span.message(
                log::Level::Info,
                &format!("Kicked service {} successfully", service_check.service_name),
            );
        }
        Ok(Err(err)) => {
            service_check.tally().record_kick_failure();
            span.message(
                log::Level::Warn,
                &format!(
                    "Failed to kick service {}: {err}",
                    service_check.service_name
                ),
            );
        }
        Err(join_err) => {
            service_check.tally().record_kick_failure();
            span.message(
                log::Level::Warn,
                &format!(
                    "Kick task for {} panicked: {join_err}",
                    service_check.service_name
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{MockFileDeleter, MockFileReader, MockFileWriter};
    use heartbeat::Heartbeat;
    use os::MockOs;
    use std::time::{Duration, SystemTime};

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: log::Event) {}
    }

    fn test_span() -> Span {
        Span::new(Arc::new(NullReporter), "test", ScopeKind::Group)
    }

    fn service_check_with(
        file_writer: MockFileWriter,
        file_deleter: MockFileDeleter,
    ) -> ServiceCheck {
        ServiceCheck::new(
            config::services::BRACT,
            &DouglasFolders::new(),
            Arc::new(MockFileReader::new()),
            Arc::new(file_writer),
            Arc::new(file_deleter),
            Arc::new(MockOs::new()),
        )
    }

    fn matches_okay(result: ServiceCheckResult) -> bool {
        matches!(result, ServiceCheckResult::Okay)
    }

    fn matches_missed_heartbeat(result: ServiceCheckResult) -> bool {
        matches!(result, ServiceCheckResult::MissedHeartbeat)
    }

    fn matches_needs_restart(result: ServiceCheckResult) -> bool {
        matches!(result, ServiceCheckResult::NeedsKick)
    }

    fn matches_check(decision: SupervisionDecision) -> bool {
        matches!(decision, SupervisionDecision::Check)
    }

    fn matches_given_up(decision: SupervisionDecision) -> bool {
        matches!(decision, SupervisionDecision::GivenUp)
    }

    #[test]
    fn test_record_heartbeat_should_reset_failure_count_and_return_okay() {
        let mut tally = ServiceTally::new();
        tally.failure_count = 2;

        assert!(matches_okay(tally.record_heartbeat()));
        assert_eq!(tally.failure_count, 0);
    }

    #[test]
    fn test_record_missed_heartbeat_should_return_missed_heartbeat_while_under_threshold() {
        let mut tally = ServiceTally::new();
        tally.recovering_since = None;

        for _ in 0..MAX_MISSED_HEARTBEATS {
            assert!(matches_missed_heartbeat(tally.record_missed_heartbeat()));
        }
        assert_eq!(tally.failure_count, MAX_MISSED_HEARTBEATS);
    }

    #[test]
    fn test_record_missed_heartbeat_should_return_needs_restart_once_threshold_exceeded() {
        let mut tally = ServiceTally::new();
        tally.recovering_since = None;

        for _ in 0..MAX_MISSED_HEARTBEATS {
            tally.record_missed_heartbeat();
        }

        assert!(matches_needs_restart(tally.record_missed_heartbeat()));
    }

    #[test]
    fn test_record_missed_heartbeat_should_not_count_failures_within_the_startup_grace() {
        let mut tally = ServiceTally::new();
        tally.recovering_since =
            Some(SystemTime::now() - Duration::from_secs(STARTUP_GRACE_SECONDS - 1));

        for _ in 0..(MAX_MISSED_HEARTBEATS + 1) {
            assert!(matches_missed_heartbeat(tally.record_missed_heartbeat()));
        }

        assert_eq!(tally.failure_count, 0);
    }

    #[test]
    fn test_record_missed_heartbeat_should_start_counting_once_the_startup_grace_elapses() {
        let mut tally = ServiceTally::new();
        tally.recovering_since =
            Some(SystemTime::now() - Duration::from_secs(STARTUP_GRACE_SECONDS + 1));

        for _ in 0..MAX_MISSED_HEARTBEATS {
            assert!(matches_missed_heartbeat(tally.record_missed_heartbeat()));
        }
        assert_eq!(tally.failure_count, MAX_MISSED_HEARTBEATS);
    }

    #[test]
    fn test_record_missed_heartbeat_should_give_a_freshly_constructed_tally_grace() {
        let mut tally = ServiceTally::new();

        assert!(matches_missed_heartbeat(tally.record_missed_heartbeat()));
        assert_eq!(tally.failure_count, 0);
    }

    #[test]
    fn test_record_kick_success_should_reset_failure_counts_and_bump_restart_count() {
        let mut tally = ServiceTally::new();
        tally.failure_count = 4;
        tally.consecutive_kick_failures = 2;
        tally.restart_count = 1;
        tally.recovering_since = None;

        tally.record_kick_success();

        assert_eq!(tally.failure_count, 0);
        assert_eq!(tally.consecutive_kick_failures, 0);
        assert_eq!(tally.restart_count, 2);
        assert!(tally.last_restart_at.is_some());
        assert!(tally.recovering_since.is_some());
    }

    #[test]
    fn test_record_kick_failure_should_count_consecutive_failures() {
        let mut tally = ServiceTally::new();

        tally.record_kick_failure();
        tally.record_kick_failure();

        assert_eq!(tally.consecutive_kick_failures, 2);
        assert!(tally.last_kick_at.is_some());
    }

    #[test]
    fn test_record_heartbeat_should_clear_consecutive_kick_failures() {
        let mut tally = ServiceTally::new();
        tally.consecutive_kick_failures = MAX_KICK_FAILURES;

        tally.record_heartbeat();

        assert_eq!(tally.consecutive_kick_failures, 0);
    }

    #[test]
    fn test_record_heartbeat_should_clear_recovering_since() {
        let mut tally = ServiceTally::new();

        tally.record_heartbeat();

        assert!(tally.recovering_since.is_none());
    }

    #[test]
    fn test_decide_should_check_a_freshly_constructed_tally() {
        let mut tally = ServiceTally::new();

        assert!(matches_check(tally.decide()));
    }

    #[test]
    fn test_decide_should_give_up_once_kicks_keep_failing() {
        let mut tally = ServiceTally::new();
        tally.consecutive_kick_failures = MAX_KICK_FAILURES;
        tally.last_kick_at = Some(SystemTime::now());

        assert!(matches_given_up(tally.decide()));
    }

    #[test]
    fn test_decide_should_recover_after_the_kick_failure_window_elapses() {
        let mut tally = ServiceTally::new();
        tally.consecutive_kick_failures = MAX_KICK_FAILURES;
        tally.last_kick_at =
            Some(SystemTime::now() - Duration::from_secs(RESTART_WINDOW_SECONDS + 1));

        assert!(matches_check(tally.decide()));
        assert_eq!(tally.consecutive_kick_failures, 0);
        assert!(tally.last_kick_at.is_none());
    }

    #[test]
    fn test_decide_should_stay_backed_off_within_the_kick_failure_window() {
        let mut tally = ServiceTally::new();
        tally.consecutive_kick_failures = MAX_KICK_FAILURES;
        tally.last_kick_at =
            Some(SystemTime::now() - Duration::from_secs(RESTART_WINDOW_SECONDS - 1));

        assert!(matches_given_up(tally.decide()));
        assert_eq!(tally.consecutive_kick_failures, MAX_KICK_FAILURES);
    }

    #[test]
    fn test_decide_should_give_up_once_restart_count_exceeds_the_max() {
        let mut tally = ServiceTally::new();
        tally.restart_count = MAX_FAILED_RESTARTS + 1;
        tally.last_restart_at = Some(SystemTime::now());

        assert!(matches_given_up(tally.decide()));
    }

    #[test]
    fn test_decide_should_reset_restart_count_after_the_restart_window_elapses() {
        let mut tally = ServiceTally::new();
        tally.restart_count = MAX_FAILED_RESTARTS + 1;
        tally.last_restart_at =
            Some(SystemTime::now() - Duration::from_secs(RESTART_WINDOW_SECONDS + 1));

        assert!(matches_check(tally.decide()));
        assert_eq!(tally.restart_count, 0);
        assert!(tally.last_restart_at.is_none());
    }

    #[test]
    fn test_decide_should_not_reset_restart_count_within_the_restart_window() {
        let mut tally = ServiceTally::new();
        tally.restart_count = MAX_FAILED_RESTARTS + 1;
        tally.last_restart_at =
            Some(SystemTime::now() - Duration::from_secs(RESTART_WINDOW_SECONDS - 1));

        assert!(matches_given_up(tally.decide()));
        assert_eq!(tally.restart_count, MAX_FAILED_RESTARTS + 1);
    }

    #[test]
    fn test_decide_should_check_immediately_after_a_successful_kick() {
        let mut tally = ServiceTally::new();
        tally.record_kick_success();

        assert!(matches_check(tally.decide()));
    }

    #[tokio::test]
    async fn test_mark_failed_should_write_the_serialized_failure_to_the_marker_path() {
        let expected_marker =
            DouglasFolders::new().supervisor_failure_marker(config::services::BRACT);
        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_all()
            .withf(move |path, contents| {
                path == expected_marker
                    && serde_json::from_str::<SupervisionFailure>(contents).is_ok_and(|failure| {
                        failure.restart_count == 2 && failure.kick_failures == 3
                    })
            })
            .times(1)
            .returning(|_, _| Ok(()));

        let check = service_check_with(file_writer, MockFileDeleter::new());

        check
            .mark_failed(&SupervisionFailure {
                service_name: config::services::BRACT.to_string(),
                gave_up_at: SystemTime::now(),
                restart_count: 2,
                kick_failures: 3,
            })
            .await
            .expect("should write the marker");
    }

    #[tokio::test]
    async fn test_clear_failure_should_delete_the_marker_path() {
        let expected_marker =
            DouglasFolders::new().supervisor_failure_marker(config::services::BRACT);
        let mut file_deleter = MockFileDeleter::new();
        file_deleter
            .expect_delete()
            .withf(move |path| path == expected_marker)
            .times(1)
            .returning(|_| Ok(()));

        let check = service_check_with(MockFileWriter::new(), file_deleter);

        check
            .clear_failure()
            .await
            .expect("should delete the marker");
    }

    #[tokio::test]
    async fn test_note_given_up_should_write_the_marker_only_once() {
        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_all()
            .times(1)
            .returning(|_, _| Ok(()));

        let check = service_check_with(file_writer, MockFileDeleter::new());
        let span = test_span();

        note_given_up(&span, &check).await;
        note_given_up(&span, &check).await;

        assert!(check.tally().failure_marked);
    }

    #[tokio::test]
    async fn test_note_recovered_should_clear_the_marker_when_one_was_written() {
        let mut file_deleter = MockFileDeleter::new();
        file_deleter.expect_delete().times(1).returning(|_| Ok(()));

        let check = service_check_with(MockFileWriter::new(), file_deleter);
        check.tally().failure_marked = true;

        note_recovered(&test_span(), &check).await;

        assert!(!check.tally().failure_marked);
    }

    #[tokio::test]
    async fn test_note_recovered_should_do_nothing_when_no_marker_was_written() {
        let mut file_deleter = MockFileDeleter::new();
        file_deleter.expect_delete().never();

        let check = service_check_with(MockFileWriter::new(), file_deleter);

        note_recovered(&test_span(), &check).await;
    }

    #[tokio::test]
    async fn test_check_one_service_should_reach_given_up_without_deadlocking() {
        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_all()
            .times(1)
            .returning(|_, _| Ok(()));

        let check = service_check_with(file_writer, MockFileDeleter::new());
        check.tally().consecutive_kick_failures = MAX_KICK_FAILURES;

        let result = tokio::time::timeout(Duration::from_secs(5), async {
            check_one_service(&test_span(), &check).await;
        })
        .await;

        assert!(
            result.is_ok(),
            "check_one_service hung instead of reaching GivenUp — regression of the \
             match-scrutinee-holds-the-tally-lock deadlock"
        );
        assert!(check.tally().failure_marked);
    }

    #[tokio::test]
    async fn test_check_one_service_should_stay_given_up_on_repeated_calls_without_deadlocking() {
        let mut file_writer = MockFileWriter::new();
        file_writer
            .expect_write_all()
            .times(1)
            .returning(|_, _| Ok(()));

        let check = service_check_with(file_writer, MockFileDeleter::new());
        check.tally().consecutive_kick_failures = MAX_KICK_FAILURES;

        let result = tokio::time::timeout(Duration::from_secs(5), async {
            check_one_service(&test_span(), &check).await;
            check_one_service(&test_span(), &check).await;
            check_one_service(&test_span(), &check).await;
        })
        .await;

        assert!(result.is_ok(), "repeated GivenUp checks hung");
        assert!(check.tally().failure_marked);
    }

    struct FixedHeartbeatReader {
        result: Result<SystemTime, HeartbeatReaderError>,
    }

    impl HeartbeatReader for FixedHeartbeatReader {
        fn read(&self) -> Result<Heartbeat, HeartbeatReaderError> {
            match &self.result {
                Ok(time) => Ok(Heartbeat {
                    pid: 1234,
                    written_at: *time,
                }),
                Err(_) => Err(HeartbeatReaderError::FileSystemError(
                    file_system::FileSystemError::IoError(std::io::Error::other("boom")),
                )),
            }
        }
    }

    #[test]
    fn test_heartbeat_age_should_be_small_for_a_recent_heartbeat() {
        let reader = FixedHeartbeatReader {
            result: Ok(SystemTime::now()),
        };

        assert!(heartbeat_age(&reader) <= Duration::from_secs(MAX_ELAPSED_SECONDS));
    }

    #[test]
    fn test_heartbeat_age_should_be_large_for_a_stale_heartbeat() {
        let written_at = SystemTime::now() - Duration::from_secs(MAX_ELAPSED_SECONDS + 1);
        let reader = FixedHeartbeatReader {
            result: Ok(written_at),
        };

        assert!(heartbeat_age(&reader) > Duration::from_secs(MAX_ELAPSED_SECONDS));
    }

    #[test]
    fn test_heartbeat_age_should_be_max_when_the_reader_errors() {
        let reader = FixedHeartbeatReader {
            result: Err(HeartbeatReaderError::FileSystemError(
                file_system::FileSystemError::IoError(std::io::Error::other("boom")),
            )),
        };

        assert_eq!(heartbeat_age(&reader), Duration::MAX);
    }

    #[test]
    fn test_kick_should_run_the_service_kick_subcommand_through_sudo_and_the_binary_link() {
        let mut os = MockOs::new();
        os.expect_execute()
            .withf(|command, args, env| {
                command == "sudo"
                    && args.as_slice() == ["-n", "/var/lib/douglas/bin/douglas", "kick", "bract"]
                    && env.is_empty()
            })
            .returning(|_, _, _| Ok(()));

        kick(
            &os,
            std::path::Path::new("/var/lib/douglas/bin/douglas"),
            "bract",
        )
        .expect("should kick the service");
    }

    #[test]
    fn test_kick_should_propagate_an_error_from_execute() {
        let mut os = MockOs::new();
        os.expect_execute()
            .returning(|_, _, _| Err(os::OsError::PidTooLarge));

        assert!(matches!(
            kick(
                &os,
                std::path::Path::new("/var/lib/douglas/bin/douglas"),
                "bract"
            ),
            Err(Error::OsError(_))
        ));
    }
}
