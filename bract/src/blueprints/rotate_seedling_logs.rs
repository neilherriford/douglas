use config::DouglasFolders;
use file_system::{EntryKind, FileRotator, Folder};
use log::{Reporter, ScopeKind, Span};
use seedbank_types::{Mount, MountType};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ROTATED_FILES: u32 = 5;

#[derive(Error, Debug)]
pub enum RotateSeedlingLogsError {
    #[error("Seedbank error {0}")]
    SeedbankError(#[from] seedbank_client::Error),
}

pub async fn execute(
    reporter: Arc<dyn Reporter>,
    seedbank_client: &dyn seedbank_client::Client,
    folder: &dyn Folder,
    rotator: &dyn FileRotator,
    douglas_folders: &DouglasFolders,
) -> Result<(), RotateSeedlingLogsError> {
    let span = Span::new(
        Arc::clone(&reporter),
        "Rotating seedling logs",
        ScopeKind::Task,
    );

    let seedling_names = seedbank_client.list().await?;

    for name in &seedling_names {
        let seedling = match seedbank_client.load(name).await {
            Ok(seedling) => seedling,
            Err(err) => {
                span.message(
                    log::Level::Warn,
                    &format!("Could not load '{name}' while sweeping for log rotation: {err}"),
                );
                continue;
            }
        };

        for (mount_name, mount) in &seedling.definition.mounts {
            if !mount.rotate_logs() {
                continue;
            }

            if !supports_rotation(mount) {
                span.message(
                    log::Level::Warn,
                    &format!(
                        "'{name}' opted mount '{mount_name}' into log rotation, but rotation \
                         isn't supported on a shared mount — skipping"
                    ),
                );
                continue;
            }

            let log_dir = douglas_folders.seedling_mount(name.as_ref(), mount_name.as_ref());
            rotate_oversized_files(&span, folder, rotator, name.as_ref(), &log_dir);
        }
    }

    Ok(())
}

fn supports_rotation(mount: &Mount) -> bool {
    !matches!(mount.kind(), MountType::PersistedShared(_))
}

fn rotate_oversized_files(
    span: &Span,
    folder: &dyn Folder,
    rotator: &dyn FileRotator,
    seedling_name: &str,
    dir: &Path,
) {
    let entries = match folder.entries(dir) {
        Ok(entries) => entries,
        Err(err) => {
            span.message(
                log::Level::Warn,
                &format!(
                    "Could not read log mount '{}' for '{seedling_name}': {err}",
                    dir.display()
                ),
            );
            return;
        }
    };

    for entry in entries {
        if entry.kind != EntryKind::File || is_already_rotated(&entry.path) {
            continue;
        }
        if entry.size >= MAX_LOG_BYTES {
            rotator.rotate(&entry.path, MAX_ROTATED_FILES);
            span.message(
                log::Level::Info,
                &format!(
                    "Rotated '{}' log file {} ({} bytes)",
                    seedling_name,
                    entry.path.display(),
                    entry.size
                ),
            );
        }
    }
}

fn is_already_rotated(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            !extension.is_empty() && extension.chars().all(|char| char.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{Entry, FileSystemError, MockFileRotator, MockFolder};
    use log::Event;
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn silent_span() -> Span {
        Span::new(Arc::new(NullReporter), "test span", ScopeKind::Task)
    }

    fn seedling_with_mounts(
        mounts: HashMap<seedbank_types::Name, Mount>,
    ) -> seedbank_types::Seedling {
        seedbank_types::Seedling {
            id: seedbank_types::Id { value: 1 },
            name: "openbao".parse().unwrap(),
            version: seedbank_types::Version(1),
            definition: seedbank_types::SeedlingDefinition::new(
                docker_types::VersionedImageName::specific("openbao", "1"),
                mounts,
                seedbank_types::Routing::None,
                seedbank_types::HealthCheck {
                    command: "true".parse().unwrap(),
                    wait_time_in_seconds: std::num::NonZeroU8::new(1).unwrap(),
                },
            ),
        }
    }

    fn file_entry(name: &str, dir: &Path, size: u64) -> Entry {
        Entry {
            name: name.to_string(),
            path: dir.join(name),
            kind: EntryKind::File,
            is_link: false,
            size,
        }
    }

    #[test]
    fn test_supports_rotation_should_allow_a_persisted_mount() {
        let mount = Mount::empty(
            MountType::Persisted,
            PathBuf::from("/var/log/douglas"),
            seedbank_types::AccessMode::Writable,
        )
        .rotating_logs();

        assert!(supports_rotation(&mount));
    }

    #[test]
    fn test_supports_rotation_should_allow_an_in_memory_mount() {
        let mount = Mount::empty(
            MountType::InMemory,
            PathBuf::from("/var/log/douglas"),
            seedbank_types::AccessMode::Writable,
        )
        .rotating_logs();

        assert!(supports_rotation(&mount));
    }

    #[test]
    fn test_supports_rotation_should_refuse_a_persisted_shared_mount() {
        let mount = Mount::empty(
            MountType::PersistedShared(vec!["other-seedling".parse().unwrap()]),
            PathBuf::from("/var/log/douglas"),
            seedbank_types::AccessMode::Writable,
        )
        .rotating_logs();

        assert!(!supports_rotation(&mount));
    }

    #[test]
    fn test_is_already_rotated_should_match_a_numeric_extension() {
        assert!(is_already_rotated(Path::new("audit.log.1")));
        assert!(is_already_rotated(Path::new("audit.log.42")));
        assert!(!is_already_rotated(Path::new("audit.log")));
        assert!(!is_already_rotated(Path::new("audit.log.bak")));
    }

    #[test]
    fn test_rotate_oversized_files_should_rotate_a_file_over_the_limit() {
        let dir = PathBuf::from("/mounts/openbao/log");
        let oversized = file_entry("audit.log", &dir, MAX_LOG_BYTES + 1);
        let expected_path = oversized.path.clone();

        let mut folder = MockFolder::new();
        folder
            .expect_entries()
            .withf(move |path| path == dir)
            .returning(move |_| Ok(vec![oversized.clone()]));

        let mut rotator = MockFileRotator::new();
        rotator
            .expect_rotate()
            .withf(move |path, max_rotated_files| {
                path == expected_path && *max_rotated_files == MAX_ROTATED_FILES
            })
            .times(1)
            .returning(|_, _| ());

        rotate_oversized_files(
            &silent_span(),
            &folder,
            &rotator,
            "openbao",
            Path::new("/mounts/openbao/log"),
        );
    }

    #[test]
    fn test_rotate_oversized_files_should_leave_a_file_under_the_limit_alone() {
        let dir = PathBuf::from("/mounts/openbao/log");

        let mut folder = MockFolder::new();
        folder
            .expect_entries()
            .returning(move |_| Ok(vec![file_entry("audit.log", &dir, 5)]));

        let mut rotator = MockFileRotator::new();
        rotator.expect_rotate().times(0);

        rotate_oversized_files(
            &silent_span(),
            &folder,
            &rotator,
            "openbao",
            Path::new("/mounts/openbao/log"),
        );
    }

    #[test]
    fn test_rotate_oversized_files_should_not_re_rotate_an_already_rotated_file() {
        let dir = PathBuf::from("/mounts/openbao/log");

        let mut folder = MockFolder::new();
        folder
            .expect_entries()
            .returning(move |_| Ok(vec![file_entry("audit.log.1", &dir, MAX_LOG_BYTES + 1)]));

        let mut rotator = MockFileRotator::new();
        rotator.expect_rotate().times(0);

        rotate_oversized_files(
            &silent_span(),
            &folder,
            &rotator,
            "openbao",
            Path::new("/mounts/openbao/log"),
        );
    }

    #[test]
    fn test_rotate_oversized_files_should_do_nothing_for_a_missing_directory() {
        let mut folder = MockFolder::new();
        folder
            .expect_entries()
            .returning(|path| Err(FileSystemError::NotFoundError(path.to_path_buf())));

        let mut rotator = MockFileRotator::new();
        rotator.expect_rotate().times(0);

        rotate_oversized_files(
            &silent_span(),
            &folder,
            &rotator,
            "openbao",
            Path::new("/mounts/openbao/log"),
        );
    }

    #[tokio::test]
    async fn test_execute_should_rotate_a_mount_that_opted_in() {
        let douglas_folders = DouglasFolders::new();
        let log_dir = douglas_folders.seedling_mount("openbao", "log");
        let oversized = file_entry("audit.log", &log_dir, MAX_LOG_BYTES + 1);

        let mut seedbank_client = seedbank_client::MockClient::new();
        seedbank_client
            .expect_list()
            .returning(|| Ok(vec!["openbao".parse().unwrap()]));
        seedbank_client.expect_load().returning(|_| {
            Ok(seedling_with_mounts(HashMap::from([(
                "log".parse().unwrap(),
                Mount::empty(
                    MountType::Persisted,
                    PathBuf::from("/var/log/douglas"),
                    seedbank_types::AccessMode::Writable,
                )
                .rotating_logs(),
            )])))
        });

        let mut folder = MockFolder::new();
        folder
            .expect_entries()
            .withf(move |path| path == log_dir)
            .returning(move |_| Ok(vec![oversized.clone()]));

        let mut rotator = MockFileRotator::new();
        rotator.expect_rotate().times(1).returning(|_, _| ());

        let reporter: Arc<dyn Reporter> = Arc::new(NullReporter);
        execute(
            reporter,
            &seedbank_client,
            &folder,
            &rotator,
            &douglas_folders,
        )
        .await
        .expect("should sweep successfully");
    }

    #[tokio::test]
    async fn test_execute_should_skip_a_mount_that_did_not_opt_in() {
        let douglas_folders = DouglasFolders::new();

        let mut seedbank_client = seedbank_client::MockClient::new();
        seedbank_client
            .expect_list()
            .returning(|| Ok(vec!["openbao".parse().unwrap()]));
        seedbank_client.expect_load().returning(|_| {
            Ok(seedling_with_mounts(HashMap::from([(
                "config".parse().unwrap(),
                Mount::empty(
                    MountType::Persisted,
                    PathBuf::from("/etc/openbao"),
                    seedbank_types::AccessMode::ReadOnly,
                ),
            )])))
        });

        let mut folder = MockFolder::new();
        folder.expect_entries().times(0);

        let mut rotator = MockFileRotator::new();
        rotator.expect_rotate().times(0);

        let reporter: Arc<dyn Reporter> = Arc::new(NullReporter);
        execute(
            reporter,
            &seedbank_client,
            &folder,
            &rotator,
            &douglas_folders,
        )
        .await
        .expect("should sweep successfully");
    }

    #[tokio::test]
    async fn test_execute_should_skip_a_persisted_shared_mount_even_if_opted_in() {
        let douglas_folders = DouglasFolders::new();

        let mut seedbank_client = seedbank_client::MockClient::new();
        seedbank_client
            .expect_list()
            .returning(|| Ok(vec!["openbao".parse().unwrap()]));
        seedbank_client.expect_load().returning(|_| {
            Ok(seedling_with_mounts(HashMap::from([(
                "log".parse().unwrap(),
                Mount::empty(
                    MountType::PersistedShared(vec!["other".parse().unwrap()]),
                    PathBuf::from("/var/log/douglas"),
                    seedbank_types::AccessMode::Writable,
                )
                .rotating_logs(),
            )])))
        });

        let mut folder = MockFolder::new();
        folder.expect_entries().times(0);

        let mut rotator = MockFileRotator::new();
        rotator.expect_rotate().times(0);

        let reporter: Arc<dyn Reporter> = Arc::new(NullReporter);
        execute(
            reporter,
            &seedbank_client,
            &folder,
            &rotator,
            &douglas_folders,
        )
        .await
        .expect("should sweep successfully");
    }
}
