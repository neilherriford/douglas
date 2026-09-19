use crate::{
    bootstrap::{HasServiceControl, KillService, OwnedServiceControl, ServiceControl, StopBract},
    cli::Presentation,
    verify::{BinaryVerifier, DouglasBinaryVerifier},
};
use async_trait::async_trait;
use blueprint::{
    Command,
    bootstrap::{execute_plan, resolve_plan},
    commands::SetOwnership,
    push_step,
};
use clap::ValueEnum;
use config::DouglasFolders;
use credentials::Credentials;
use file_system::{
    FileReader, FileRenamer, FileSystemError, Inspect, Links, Permissions, UnixFileRenamer,
    UnixInspect, UnixLinks, UnixPermissions, path_to_string,
};
use log::{Level, Outcome, Reporter, ScopeKind, Span};
use os::Os;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum UpgradeError {
    #[error("Must be root to proceed")]
    MustBeRoot,
    #[error("No executable at given path")]
    Missing(PathBuf),
    #[error("Not a valid douglas executable")]
    InvalidExecutable,
    #[error("The target must be a higher version than the current version")]
    InvalidUpgrade,
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

struct Context<'a> {
    douglas_folders: DouglasFolders,
    service_control: ServiceControl<'a>,
    file_renamer: &'a dyn FileRenamer,
    links: &'a dyn Links,
    permissions: &'a dyn Permissions,
}

impl HasServiceControl for Context<'_> {
    fn service_control(&self) -> &ServiceControl<'_> {
        &self.service_control
    }
}

impl blueprint::HasPermissions for Context<'_> {
    fn permissions(&self) -> &dyn Permissions {
        self.permissions
    }
}

enum State {
    NotRoot,
    Missing,
    NotNewer,
    Upgradable {
        is_marked_as_executable: bool,
        is_owned_by_douglas_admin: bool,
    },
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    inspect: &'a dyn Inspect,
    binary_verifier: &'a dyn BinaryVerifier,
    permissions: &'a dyn Permissions,
}

impl StateObserver<'_> {
    pub fn discover(&mut self, span: &Span, path: &Path) -> Result<State, UpgradeError> {
        let guard = span
            .create_child(
                "Upgrading douglas system, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        if !self.credentials.is_root() {
            return guard.finish(Ok(State::NotRoot));
        }

        if !self.inspect.exists(path) {
            return guard.finish(Ok(State::Missing));
        }

        let external_version = match self.binary_verifier.get_external_version(path) {
            Ok(version) => version,
            Err(err) => {
                guard
                    .span()
                    .message(Level::Warn, &format!("Invalid douglas executable: {err}"));
                return guard.finish(Err(UpgradeError::InvalidExecutable));
            }
        };

        let internal_version = match self.binary_verifier.get_internal_version() {
            Ok(version) => version,
            Err(err) => {
                guard.span().message(
                    Level::Warn,
                    &format!("Could not determine internal version: {err}"),
                );
                return guard.finish(Err(UpgradeError::InvalidExecutable));
            }
        };

        if external_version <= internal_version {
            return guard.finish(Ok(State::NotNewer));
        }

        let is_marked_as_executable = self
            .permissions
            .get_mode(path)
            .is_ok_and(|value| value.is_executable_by_owner() || value.is_executable_by_group());

        let is_owned_by_douglas_admin = self
            .permissions
            .get_user_and_group_ownership(path)
            .is_ok_and(|(user, group)| {
                user == credentials::ROOT_USER_NAME
                    && group == credentials::well_known::DOUGLAS_ADMIN_GROUP
            });

        guard.finish(Ok(State::Upgradable {
            is_marked_as_executable,
            is_owned_by_douglas_admin,
        }))
    }
}

fn create_plan<'a>(
    state: &State,
    path: &Path,
    presentation: Presentation,
) -> Result<Vec<Step<'a>>, UpgradeError> {
    let (is_marked_as_executable, is_owned_by_douglas_admin) = match *state {
        State::NotRoot => return Err(UpgradeError::MustBeRoot),
        State::Missing => return Err(UpgradeError::Missing(path.to_path_buf())),
        State::NotNewer => return Err(UpgradeError::InvalidUpgrade),
        State::Upgradable {
            is_marked_as_executable,
            is_owned_by_douglas_admin,
        } => (is_marked_as_executable, is_owned_by_douglas_admin),
    };

    let mut result = Vec::new();

    if !is_marked_as_executable {
        push_step(&mut result, MarkExecutable::new(path));
    }

    if !is_owned_by_douglas_admin {
        push_step(
            &mut result,
            SetOwnership::new(
                path.to_path_buf(),
                credentials::ROOT_USER_NAME,
                credentials::well_known::DOUGLAS_ADMIN_GROUP,
            ),
        );
    }

    push_step(&mut result, KillService::new(config::services::WOODWARD));
    push_step(&mut result, StopBract::new(false));
    push_step(&mut result, KillService::new(config::services::RESIN));
    push_step(&mut result, KillService::new(config::services::SEEDBANK));
    push_step(&mut result, OverwriteDouglasExecutable::new(path));
    push_step(&mut result, ReplaceProcess::new(presentation));

    Ok(result)
}

#[derive(Debug)]
struct MarkExecutable {
    path: PathBuf,
}

impl MarkExecutable {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }
}

impl std::fmt::Display for MarkExecutable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pretty_path = path_to_string(&self.path);
        write!(f, "Mark {pretty_path} executable")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for MarkExecutable {
    fn name(&self) -> String {
        let pretty_path = path_to_string(&self.path);
        format!("Mark {pretty_path} executable")
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Marking executable…", ScopeKind::Step)
            .start_guard();

        let mut new_mode = context.permissions.get_mode(&self.path)?;
        new_mode = new_mode.set_executable_by_group(true);
        new_mode = new_mode.set_executable_by_owner(true);

        context.permissions.change_mode(&self.path, &new_mode)?;

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

#[derive(Debug)]
struct OverwriteDouglasExecutable {
    path: PathBuf,
}

impl OverwriteDouglasExecutable {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }
}

impl std::fmt::Display for OverwriteDouglasExecutable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Overwrite current version with new version")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for OverwriteDouglasExecutable {
    fn name(&self) -> String {
        "Overwrite current version with new version".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                "Overwriting current version with new version…",
                ScopeKind::Step,
            )
            .start_guard();

        let link = context.douglas_folders.binary_link();
        let target = context
            .douglas_folders
            .binary_dir()
            .join(context.links.follow_symbolic(&link)?);
        context.file_renamer.rename(&self.path, &target)?;

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

#[derive(Debug)]
struct ReplaceProcess {
    presentation: Presentation,
}

impl ReplaceProcess {
    pub fn new(presentation: Presentation) -> Self {
        Self { presentation }
    }
}

fn start_args(presentation: Presentation) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(style) = presentation.console_style()
        && let Some(value) = style.to_possible_value()
    {
        args.push("--output-style".to_string());
        args.push(value.get_name().to_string());
    }
    args.push("start".to_string());
    args
}

impl std::fmt::Display for ReplaceProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Replace current process with new version")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for ReplaceProcess {
    fn name(&self) -> String {
        "Replace current process with new version".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                "Replacing current process with new version…",
                ScopeKind::Step,
            )
            .start_guard();

        if self.presentation == Presentation::Interactive {
            let _ = crate::cli_reporter::restore_term();
        }

        context.service_control.os.replace_process(
            &path_to_string(context.douglas_folders.binary_link()),
            start_args(self.presentation),
            vec![],
        )?;

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

pub(crate) struct Dependencies {
    pub credentials: Arc<dyn Credentials>,
    pub os: Arc<dyn Os>,
    pub douglas_folders: DouglasFolders,
    pub file_reader: Arc<dyn FileReader>,
}

pub async fn perform(
    reporter: Arc<dyn Reporter>,
    plan_only: bool,
    deps: Dependencies,
    path: &Path,
    presentation: Presentation,
) -> bool {
    let guard = Span::new(
        Arc::clone(&reporter),
        "Upgrading douglas system",
        log::ScopeKind::Group,
    )
    .start_guard();

    let permissions = UnixPermissions::new();
    let binary_verifier =
        DouglasBinaryVerifier::new(Arc::clone(&deps.os), Arc::clone(&deps.file_reader));

    let mut state_observer = StateObserver {
        credentials: deps.credentials.as_ref(),
        inspect: &UnixInspect::new(),
        binary_verifier: &binary_verifier,
        permissions: &permissions,
    };

    let state = match state_observer.discover(guard.span(), path) {
        Ok(state) => state,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            return false;
        }
    };

    let Some(service_control) = OwnedServiceControl::build(
        &guard,
        Arc::clone(&deps.os),
        &deps.douglas_folders,
        Arc::clone(&deps.file_reader),
    )
    .await
    else {
        return false;
    };

    let file_renamer = UnixFileRenamer::new();
    let links = UnixLinks::new();

    let plan = match resolve_plan(guard.span(), create_plan(&state, path, presentation)) {
        Ok(plan) => plan,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            guard.finish_with_outcome(log::Outcome::Failed);
            return false;
        }
    };

    if plan_only {
        guard.finish_with_outcome(log::Outcome::Ok);
        return true;
    }

    let mut context = Context {
        douglas_folders: deps.douglas_folders,
        service_control: service_control.borrow(),
        file_renamer: &file_renamer,
        links: &links,
        permissions: &permissions,
    };

    let result = execute_plan(guard.span(), plan, &mut context, |_reason| ()).await;

    if result.is_ok() {
        guard.finish_with_outcome(Outcome::Ok);
        true
    } else {
        guard.finish_with_outcome(Outcome::Failed);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{MockBinaryVerifier, Version};
    use credentials::MockCredentials;
    use file_system::{MockFileRenamer, MockInspect, MockLinks, MockPermissions};
    use heartbeat::HeartbeatReaderFactory;
    use os::MockOs;
    use std::sync::Mutex;

    struct CapturingReporter {
        messages: Mutex<Vec<String>>,
    }

    impl CapturingReporter {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                messages: Mutex::new(Vec::new()),
            })
        }

        fn messages(&self) -> Vec<String> {
            let Ok(messages) = self.messages.lock() else {
                panic!("messages mutex poisoned");
            };
            messages.clone()
        }
    }

    impl Reporter for CapturingReporter {
        fn emit(&self, event: log::Event) {
            if let log::EventKind::Message { text, .. } = event.kind {
                let Ok(mut messages) = self.messages.lock() else {
                    panic!("messages mutex poisoned");
                };
                messages.push(text);
            }
        }
    }

    fn test_guard(reporter: Arc<dyn Reporter>) -> log::ScopeGuard {
        Span::new(reporter, "test", ScopeKind::Group).start_guard()
    }

    fn version(major: u8, minor: u8, patch: u8) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    mod create_plan_tests {
        use super::*;

        fn candidate_path() -> PathBuf {
            PathBuf::from("/tmp/candidate-douglas")
        }

        #[test]
        fn test_should_error_when_not_root() {
            let state = State::NotRoot;

            let result = create_plan(&state, &candidate_path(), Presentation::Plain);

            assert!(matches!(result, Err(UpgradeError::MustBeRoot)));
        }

        #[test]
        fn test_should_error_when_binary_does_not_exist() {
            let state = State::Missing;

            let result = create_plan(&state, &candidate_path(), Presentation::Plain);

            assert!(matches!(result, Err(UpgradeError::Missing(path)) if path == candidate_path()));
        }

        #[test]
        fn test_should_error_when_not_a_higher_version() {
            let state = State::NotNewer;

            let result = create_plan(&state, &candidate_path(), Presentation::Plain);

            assert!(matches!(result, Err(UpgradeError::InvalidUpgrade)));
        }

        #[test]
        fn test_should_skip_mark_executable_and_set_ownership_when_already_correct() {
            let state = State::Upgradable {
                is_marked_as_executable: true,
                is_owned_by_douglas_admin: true,
            };

            let Ok(steps) = create_plan(&state, &candidate_path(), Presentation::Plain) else {
                panic!("should plan");
            };
            let descriptions: Vec<String> =
                steps.iter().map(std::string::ToString::to_string).collect();

            assert_eq!(
                descriptions,
                vec![
                    "Kill service woodward".to_string(),
                    "Stopping Bract".to_string(),
                    "Kill service resin".to_string(),
                    "Kill service seedbank".to_string(),
                    "Overwrite current version with new version".to_string(),
                    "Replace current process with new version".to_string(),
                ]
            );
        }

        #[test]
        fn test_should_mark_executable_and_set_ownership_first_when_needed() {
            let state = State::Upgradable {
                is_marked_as_executable: false,
                is_owned_by_douglas_admin: false,
            };

            let Ok(steps) = create_plan(&state, &candidate_path(), Presentation::Plain) else {
                panic!("should plan");
            };
            let descriptions: Vec<String> =
                steps.iter().map(std::string::ToString::to_string).collect();

            assert_eq!(
                descriptions,
                vec![
                    "Mark /tmp/candidate-douglas executable".to_string(),
                    "Set ownership on '/tmp/candidate-douglas' to user 'root' group \
                     'douglas-admin'"
                        .to_string(),
                    "Kill service woodward".to_string(),
                    "Stopping Bract".to_string(),
                    "Kill service resin".to_string(),
                    "Kill service seedbank".to_string(),
                    "Overwrite current version with new version".to_string(),
                    "Replace current process with new version".to_string(),
                ]
            );
        }
    }

    mod start_args_tests {
        use super::*;

        #[test]
        fn test_should_start_without_a_style_when_interactive() {
            assert_eq!(start_args(Presentation::Interactive), vec!["start"]);
        }

        #[test]
        fn test_should_pass_plain_through_to_start() {
            assert_eq!(
                start_args(Presentation::Plain),
                vec!["--output-style", "plain", "start"]
            );
        }

        #[test]
        fn test_should_pass_json_through_to_start() {
            assert_eq!(
                start_args(Presentation::Json),
                vec!["--output-style", "json", "start"]
            );
        }
    }

    mod discover_tests {
        use super::*;

        #[test]
        fn test_should_return_default_state_when_not_root() {
            let mut credentials = MockCredentials::new();
            credentials.expect_is_root().returning(|| false);
            let inspect = MockInspect::new();
            let binary_verifier = MockBinaryVerifier::new();
            let permissions = MockPermissions::new();

            let mut observer = StateObserver {
                credentials: &credentials,
                inspect: &inspect,
                binary_verifier: &binary_verifier,
                permissions: &permissions,
            };
            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);

            let Ok(state) = observer.discover(guard.span(), Path::new("/tmp/candidate")) else {
                panic!("should discover");
            };

            assert!(matches!(state, State::NotRoot));
        }

        #[test]
        fn test_should_stop_at_exists_when_binary_is_missing() {
            let mut credentials = MockCredentials::new();
            credentials.expect_is_root().returning(|| true);
            let mut inspect = MockInspect::new();
            inspect.expect_exists().returning(|_| false);
            let binary_verifier = MockBinaryVerifier::new();
            let permissions = MockPermissions::new();

            let mut observer = StateObserver {
                credentials: &credentials,
                inspect: &inspect,
                binary_verifier: &binary_verifier,
                permissions: &permissions,
            };
            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);

            let Ok(state) = observer.discover(guard.span(), Path::new("/tmp/candidate")) else {
                panic!("should discover");
            };

            assert!(matches!(state, State::Missing));
        }

        #[test]
        fn test_should_error_when_the_candidate_binary_does_not_verify() {
            let mut credentials = MockCredentials::new();
            credentials.expect_is_root().returning(|| true);
            let mut inspect = MockInspect::new();
            inspect.expect_exists().returning(|_| true);
            let mut binary_verifier = MockBinaryVerifier::new();
            binary_verifier
                .expect_get_external_version()
                .returning(|_| Err(crate::verify::VerifyError::UnknownInternalVersion));
            let permissions = MockPermissions::new();

            let mut observer = StateObserver {
                credentials: &credentials,
                inspect: &inspect,
                binary_verifier: &binary_verifier,
                permissions: &permissions,
            };
            let reporter = CapturingReporter::new();
            let guard = test_guard(Arc::clone(&reporter) as Arc<dyn Reporter>);

            let result = observer.discover(guard.span(), Path::new("/tmp/candidate"));

            assert!(matches!(result, Err(UpgradeError::InvalidExecutable)));
            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Invalid douglas executable"))
            );
        }

        #[test]
        fn test_should_error_when_the_internal_version_cannot_be_determined() {
            let mut credentials = MockCredentials::new();
            credentials.expect_is_root().returning(|| true);
            let mut inspect = MockInspect::new();
            inspect.expect_exists().returning(|_| true);
            let mut binary_verifier = MockBinaryVerifier::new();
            binary_verifier
                .expect_get_external_version()
                .returning(|_| Ok(version(1, 0, 0)));
            binary_verifier
                .expect_get_internal_version()
                .returning(|| Err(crate::verify::VerifyError::UnknownInternalVersion));
            let permissions = MockPermissions::new();

            let mut observer = StateObserver {
                credentials: &credentials,
                inspect: &inspect,
                binary_verifier: &binary_verifier,
                permissions: &permissions,
            };
            let reporter = CapturingReporter::new();
            let guard = test_guard(Arc::clone(&reporter) as Arc<dyn Reporter>);

            let result = observer.discover(guard.span(), Path::new("/tmp/candidate"));

            assert!(matches!(result, Err(UpgradeError::InvalidExecutable)));
            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Could not determine internal version"))
            );
        }

        #[test]
        fn test_should_not_flag_a_higher_version_when_candidate_is_not_newer() {
            let mut credentials = MockCredentials::new();
            credentials.expect_is_root().returning(|| true);
            let mut inspect = MockInspect::new();
            inspect.expect_exists().returning(|_| true);
            let mut binary_verifier = MockBinaryVerifier::new();
            binary_verifier
                .expect_get_external_version()
                .returning(|_| Ok(version(1, 0, 0)));
            binary_verifier
                .expect_get_internal_version()
                .returning(|| Ok(version(1, 0, 0)));
            let permissions = MockPermissions::new();

            let mut observer = StateObserver {
                credentials: &credentials,
                inspect: &inspect,
                binary_verifier: &binary_verifier,
                permissions: &permissions,
            };
            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);

            let Ok(state) = observer.discover(guard.span(), Path::new("/tmp/candidate")) else {
                panic!("should discover");
            };

            assert!(matches!(state, State::NotNewer));
        }

        #[test]
        fn test_should_report_executable_and_ownership_when_correct() {
            let mut credentials = MockCredentials::new();
            credentials.expect_is_root().returning(|| true);
            let mut inspect = MockInspect::new();
            inspect.expect_exists().returning(|_| true);
            let mut binary_verifier = MockBinaryVerifier::new();
            binary_verifier
                .expect_get_external_version()
                .returning(|_| Ok(version(2, 0, 0)));
            binary_verifier
                .expect_get_internal_version()
                .returning(|| Ok(version(1, 0, 0)));
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_mode()
                .returning(|_| Ok(file_system::Modes::OwnerReadWriteExecute));
            permissions
                .expect_get_user_and_group_ownership()
                .returning(|_| {
                    Ok((
                        credentials::ROOT_USER_NAME.to_string(),
                        credentials::well_known::DOUGLAS_ADMIN_GROUP.to_string(),
                    ))
                });

            let mut observer = StateObserver {
                credentials: &credentials,
                inspect: &inspect,
                binary_verifier: &binary_verifier,
                permissions: &permissions,
            };
            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);

            let Ok(state) = observer.discover(guard.span(), Path::new("/tmp/candidate")) else {
                panic!("should discover");
            };

            assert!(matches!(
                state,
                State::Upgradable {
                    is_marked_as_executable: true,
                    is_owned_by_douglas_admin: true,
                }
            ));
        }

        #[test]
        fn test_should_report_not_executable_and_not_owned_when_incorrect() {
            let mut credentials = MockCredentials::new();
            credentials.expect_is_root().returning(|| true);
            let mut inspect = MockInspect::new();
            inspect.expect_exists().returning(|_| true);
            let mut binary_verifier = MockBinaryVerifier::new();
            binary_verifier
                .expect_get_external_version()
                .returning(|_| Ok(version(2, 0, 0)));
            binary_verifier
                .expect_get_internal_version()
                .returning(|| Ok(version(1, 0, 0)));
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_mode()
                .returning(|_| Ok(file_system::Modes::OwnerReadWrite));
            permissions
                .expect_get_user_and_group_ownership()
                .returning(|_| Ok(("dev".to_string(), "dev".to_string())));

            let mut observer = StateObserver {
                credentials: &credentials,
                inspect: &inspect,
                binary_verifier: &binary_verifier,
                permissions: &permissions,
            };
            let reporter = CapturingReporter::new();
            let guard = test_guard(reporter);

            let Ok(state) = observer.discover(guard.span(), Path::new("/tmp/candidate")) else {
                panic!("should discover");
            };

            assert!(matches!(
                state,
                State::Upgradable {
                    is_marked_as_executable: false,
                    is_owned_by_douglas_admin: false,
                }
            ));
        }
    }

    mod command_integration_tests {
        use super::*;

        fn test_context<'a>(
            os: &'a dyn Os,
            file_renamer: &'a dyn FileRenamer,
            links: &'a dyn Links,
            permissions: &'a dyn Permissions,
            heartbeat_reader_factory: &'a dyn HeartbeatReaderFactory,
            docker_client: &'a dyn docker::client::Client,
            bract_client: &'a dyn bract_client::Client,
        ) -> Context<'a> {
            Context {
                douglas_folders: DouglasFolders::default(),
                service_control: ServiceControl {
                    os,
                    heartbeat_reader_factory,
                    bract_client,
                    docker_client,
                },
                file_renamer,
                links,
                permissions,
            }
        }

        #[tokio::test]
        async fn test_mark_executable_run_should_set_owner_and_group_execute() {
            let os = MockOs::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_mode()
                .returning(|_| Ok(file_system::Modes::OwnerReadWrite));
            permissions
                .expect_change_mode()
                .withf(|_, mode| mode.is_executable_by_owner() && mode.is_executable_by_group())
                .returning(|_, _| Ok(()));
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                &file_renamer,
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = MarkExecutable::new(Path::new("/tmp/candidate-douglas"));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_rename_the_candidate_onto_the_links_real_target() {
            let os = MockOs::new();
            let mut file_renamer = MockFileRenamer::new();
            file_renamer
                .expect_rename()
                .withf(|from, to| {
                    from == Path::new("/tmp/candidate-douglas")
                        && to == Path::new("/home/dev/douglas")
                })
                .times(1)
                .returning(|_, _| Ok(()));
            let mut links = MockLinks::new();
            let expected_link = DouglasFolders::default().binary_link();
            links
                .expect_follow_symbolic()
                .withf(move |path| path == expected_link)
                .returning(|_| Ok(PathBuf::from("/home/dev/douglas")));
            let permissions = MockPermissions::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                &file_renamer,
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = OverwriteDouglasExecutable::new(Path::new("/tmp/candidate-douglas"));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_not_rename_when_the_binary_link_cannot_be_followed() {
            let os = MockOs::new();
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().times(0);
            let mut links = MockLinks::new();
            links.expect_follow_symbolic().returning(|path| {
                Err(file_system::FileSystemError::NotFoundError(
                    path.to_path_buf(),
                ))
            });
            let permissions = MockPermissions::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                &file_renamer,
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = OverwriteDouglasExecutable::new(Path::new("/tmp/candidate-douglas"));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_replace_process_run_should_start_plain_from_the_binary_link() {
            let mut os = MockOs::new();
            let expected_path = DouglasFolders::default().binary_link();
            let expected_path = path_to_string(&expected_path);
            os.expect_replace_process()
                .withf(move |command, args, _env| {
                    command == expected_path
                        && *args
                            == vec![
                                "--output-style".to_string(),
                                "plain".to_string(),
                                "start".to_string(),
                            ]
                })
                .returning(|_, _, _| Ok(()));
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                &file_renamer,
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = ReplaceProcess::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_replace_process_run_should_fail_when_the_os_call_fails() {
            let mut os = MockOs::new();
            os.expect_replace_process()
                .returning(|_, _, _| Err(os::OsError::PidTooLarge));
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                &file_renamer,
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = ReplaceProcess::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }
    }
}
