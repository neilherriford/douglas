use crate::{
    bootstrap::{
        HasServiceControl, KillService, OwnedServiceControl, ServiceControl, StopBract, retention,
    },
    cli::Presentation,
    verify::{BinaryVerifier, DouglasBinaryVerifier, Version},
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
    FileCopier, FileDeleter, FileReader, FileRenamer, FileSystemError, Folder, Inspect, Links,
    Permissions, UnixFileCopier, UnixFileDeleter, UnixFileRenamer, UnixFolder, UnixInspect,
    UnixLinks, UnixPermissions, path_to_string,
};
use log::{Level, Outcome, Reporter, ScopeGuard, ScopeKind, Span};
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

struct FileOperations<'a> {
    renamer: &'a dyn FileRenamer,
    copier: &'a dyn FileCopier,
    deleter: &'a dyn FileDeleter,
    folder: &'a dyn Folder,
}

struct Context<'a> {
    douglas_folders: DouglasFolders,
    service_control: ServiceControl<'a>,
    files: FileOperations<'a>,
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
        current: Version,
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

        let external = match self.binary_verifier.get_external_release(path) {
            Ok(release) => release,
            Err(err) => {
                guard
                    .span()
                    .message(Level::Warn, &format!("Invalid douglas executable: {err}"));
                return guard.finish(Err(UpgradeError::InvalidExecutable));
            }
        };

        let internal = match self.binary_verifier.get_internal_release() {
            Ok(release) => release,
            Err(err) => {
                guard.span().message(
                    Level::Warn,
                    &format!("Could not determine internal version: {err}"),
                );
                return guard.finish(Err(UpgradeError::InvalidExecutable));
            }
        };

        if external.version <= internal.version {
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
            current: internal.version,
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
    let (current, is_marked_as_executable, is_owned_by_douglas_admin) = match *state {
        State::NotRoot => return Err(UpgradeError::MustBeRoot),
        State::Missing => return Err(UpgradeError::Missing(path.to_path_buf())),
        State::NotNewer => return Err(UpgradeError::InvalidUpgrade),
        State::Upgradable {
            current,
            is_marked_as_executable,
            is_owned_by_douglas_admin,
        } => (current, is_marked_as_executable, is_owned_by_douglas_admin),
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

    push_step(&mut result, RetainPreviousBinary::new(current));
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
struct RetainPreviousBinary {
    current: Version,
}

impl RetainPreviousBinary {
    pub fn new(current: Version) -> Self {
        Self { current }
    }

    fn copy_aside(
        context: &Context<'_>,
        target: &Path,
        partial: &Path,
        retained: &Path,
    ) -> Result<(), FileSystemError> {
        context.files.copier.copy(target, partial)?;
        let (user, group) = context.permissions.get_user_and_group_ownership(target)?;
        context
            .permissions
            .change_user_and_group_ownership(partial, &user, &group)?;
        context.files.renamer.rename(partial, retained)
    }

    fn prune(guard: &ScopeGuard, context: &Context<'_>, binary_dir: &Path) {
        let entries = match context.files.folder.entries(binary_dir) {
            Ok(entries) => entries,
            Err(err) => {
                guard.span().message(
                    Level::Warn,
                    &format!("Could not list retained versions to prune them: {err}"),
                );
                return;
            }
        };
        let names: Vec<String> = entries.into_iter().map(|entry| entry.name).collect();

        for name in retention::expired(&names, retention::RETAINED_COUNT) {
            if let Err(err) = context.files.deleter.delete(&binary_dir.join(&name)) {
                guard.span().message(
                    Level::Warn,
                    &format!("Could not remove the old retained version {name}: {err}"),
                );
            }
        }
    }
}

impl std::fmt::Display for RetainPreviousBinary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keep the current version for rollback")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for RetainPreviousBinary {
    fn name(&self) -> String {
        "Keep the current version for rollback".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Keeping the current version for rollback…", ScopeKind::Step)
            .start_guard();

        let binary_dir = context.douglas_folders.binary_dir();
        let link = context.douglas_folders.binary_link();
        let target = binary_dir.join(context.links.follow_symbolic(&link)?);
        let retained = retention::retained_path(&binary_dir, self.current);
        let partial = retention::partial_path(&retained)
            .ok_or_else(|| FileSystemError::InvalidPath(retained.clone()))?;

        if let Err(err) = Self::copy_aside(context, &target, &partial, &retained) {
            let _ = context.files.deleter.delete(&partial);
            return Err(err.into());
        }

        Self::prune(&guard, context, &binary_dir);

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

    fn install_from_copy(
        &self,
        context: &Context<'_>,
        staging: &Path,
        target: &Path,
    ) -> Result<(), FileSystemError> {
        context.files.copier.copy(&self.path, staging)?;
        let (user, group) = context
            .permissions
            .get_user_and_group_ownership(&self.path)?;
        context
            .permissions
            .change_user_and_group_ownership(staging, &user, &group)?;
        context.files.renamer.rename(staging, target)
    }

    fn copy_into_place(
        &self,
        guard: &ScopeGuard,
        context: &Context<'_>,
        target: &Path,
    ) -> Result<(), FileSystemError> {
        let staging = staging_path(target)?;

        if let Err(err) = self.install_from_copy(context, &staging, target) {
            let _ = context.files.deleter.delete(&staging);
            return Err(err);
        }

        if let Err(err) = context.files.deleter.delete(&self.path) {
            guard.span().message(
                Level::Warn,
                &format!(
                    "Installed the new version but could not remove {}: {err}",
                    path_to_string(&self.path)
                ),
            );
        }
        Ok(())
    }
}

fn staging_path(target: &Path) -> Result<PathBuf, FileSystemError> {
    let name = target
        .file_name()
        .ok_or_else(|| FileSystemError::InvalidPath(target.to_path_buf()))?;
    Ok(target.with_file_name(format!("{}.upgrade", name.to_string_lossy())))
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
        match context.files.renamer.rename(&self.path, &target) {
            Err(err) if err.is_cross_device() => self.copy_into_place(&guard, context, &target)?,
            result => result?,
        }

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
    let file_copier = UnixFileCopier::new();
    let file_deleter = UnixFileDeleter::new();
    let folder = UnixFolder::new();
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
        files: FileOperations {
            renamer: &file_renamer,
            copier: &file_copier,
            deleter: &file_deleter,
            folder: &folder,
        },
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
    use crate::verify::{MockBinaryVerifier, Release, Version};
    use credentials::MockCredentials;
    use file_system::{
        MockFileCopier, MockFileDeleter, MockFileRenamer, MockFolder, MockInspect, MockLinks,
        MockPermissions,
    };
    use heartbeat::HeartbeatReaderFactory;
    use mockall::Sequence;
    use os::MockOs;
    use release::ReleaseMetadata;
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

    fn release(major: u8, minor: u8, patch: u8) -> Release {
        Release {
            version: version(major, minor, patch),
            metadata: ReleaseMetadata::current(),
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
                current: version(0, 0, 1),
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
                    "Keep the current version for rollback".to_string(),
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
                current: version(0, 0, 1),
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
                    "Keep the current version for rollback".to_string(),
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

    mod staging_path_tests {
        use super::*;

        #[test]
        fn test_should_place_the_staging_file_next_to_the_target() {
            let result = staging_path(Path::new("/home/dev/douglas"));

            assert!(matches!(result, Ok(path) if path == Path::new("/home/dev/douglas.upgrade")));
        }

        #[test]
        fn test_should_fail_when_the_target_has_no_file_name() {
            let result = staging_path(Path::new("/"));

            assert!(matches!(result, Err(FileSystemError::InvalidPath(_))));
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
                .expect_get_external_release()
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
                .expect_get_external_release()
                .returning(|_| Ok(release(1, 0, 0)));
            binary_verifier
                .expect_get_internal_release()
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
                .expect_get_external_release()
                .returning(|_| Ok(release(1, 0, 0)));
            binary_verifier
                .expect_get_internal_release()
                .returning(|| Ok(release(1, 0, 0)));
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
                .expect_get_external_release()
                .returning(|_| Ok(release(2, 0, 0)));
            binary_verifier
                .expect_get_internal_release()
                .returning(|| Ok(release(1, 0, 0)));
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
                    current: Version {
                        major: 1,
                        minor: 0,
                        patch: 0,
                    },
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
                .expect_get_external_release()
                .returning(|_| Ok(release(2, 0, 0)));
            binary_verifier
                .expect_get_internal_release()
                .returning(|| Ok(release(1, 0, 0)));
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
                    current: Version {
                        major: 1,
                        minor: 0,
                        patch: 0,
                    },
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
            files: FileOperations<'a>,
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
                files,
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
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
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

        const CANDIDATE: &str = "/tmp/candidate-douglas";
        const TARGET: &str = "/home/dev/douglas";
        const STAGING: &str = "/home/dev/douglas.upgrade";

        fn cross_device() -> FileSystemError {
            FileSystemError::IoErrorAtPath {
                path: PathBuf::from(CANDIDATE),
                error: std::io::Error::from(std::io::ErrorKind::CrossesDevices),
            }
        }

        fn permission_denied() -> FileSystemError {
            FileSystemError::IoErrorAtPath {
                path: PathBuf::from(CANDIDATE),
                error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            }
        }

        fn links_to_target() -> MockLinks {
            let mut links = MockLinks::new();
            let expected_link = DouglasFolders::default().binary_link();
            links
                .expect_follow_symbolic()
                .withf(move |path| path == expected_link)
                .returning(|_| Ok(PathBuf::from(TARGET)));
            links
        }

        fn candidate_is_root_owned_by_douglas_admin() -> MockPermissions {
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_user_and_group_ownership()
                .withf(|path| path == Path::new(CANDIDATE))
                .returning(|_| Ok(("root".to_string(), "douglas-admin".to_string())));
            permissions
        }

        fn binary_dir() -> PathBuf {
            DouglasFolders::default().binary_dir()
        }

        fn retained() -> PathBuf {
            binary_dir().join("douglas-0.0.4")
        }

        fn partial() -> PathBuf {
            binary_dir().join("douglas-0.0.4.partial")
        }

        fn target_is_owned_by_dev() -> MockPermissions {
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_user_and_group_ownership()
                .withf(|path| path == Path::new(TARGET))
                .returning(|_| Ok(("dev".to_string(), "dev-group".to_string())));
            permissions
        }

        fn entry(name: &str) -> file_system::Entry {
            file_system::Entry {
                name: name.to_string(),
                path: binary_dir().join(name),
                kind: file_system::EntryKind::File,
                is_link: false,
                size: 1,
            }
        }

        fn folder_listing(names: &[&str]) -> MockFolder {
            let entries: Vec<file_system::Entry> = names.iter().map(|name| entry(name)).collect();
            let mut folder = MockFolder::new();
            folder
                .expect_entries()
                .withf(|path| path == binary_dir())
                .returning(move |_| Ok(entries.clone()));
            folder
        }

        #[tokio::test]
        async fn test_retain_run_should_copy_the_target_aside_hand_it_the_same_owner_and_rename_it_into_place()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier
                .expect_copy()
                .withf(|from, to| from == Path::new(TARGET) && to == partial())
                .times(1)
                .returning(|_, _| Ok(()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer
                .expect_rename()
                .withf(|from, to| from == partial() && to == retained())
                .times(1)
                .returning(|_, _| Ok(()));
            let mut permissions = target_is_owned_by_dev();
            permissions
                .expect_change_user_and_group_ownership()
                .withf(|path, user, group| {
                    path == partial() && user == "dev" && group == "dev-group"
                })
                .times(1)
                .returning(|_, _, _| Ok(()));
            let file_deleter = MockFileDeleter::new();
            let folder = folder_listing(&["douglas", "douglas-0.0.4"]);
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RetainPreviousBinary::new(version(0, 0, 4));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_retain_run_should_delete_only_the_versions_beyond_the_newest_three() {
            let mut file_copier = MockFileCopier::new();
            file_copier.expect_copy().returning(|_, _| Ok(()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().returning(|_, _| Ok(()));
            let mut permissions = target_is_owned_by_dev();
            permissions
                .expect_change_user_and_group_ownership()
                .returning(|_, _, _| Ok(()));
            let mut file_deleter = MockFileDeleter::new();
            file_deleter
                .expect_delete()
                .withf(|path| path == binary_dir().join("douglas-0.0.1"))
                .times(1)
                .returning(|_| Ok(()));
            file_deleter
                .expect_delete()
                .withf(|path| path == binary_dir().join("douglas-0.0.2"))
                .times(1)
                .returning(|_| Ok(()));
            let folder = folder_listing(&[
                "douglas",
                "douglas-0.0.1",
                "douglas-0.0.2",
                "douglas-0.0.3",
                "douglas-0.0.4",
                "douglas-0.0.5",
                "install-marker.json",
            ]);
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RetainPreviousBinary::new(version(0, 0, 4));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_retain_run_should_still_succeed_and_warn_when_the_retained_versions_cannot_be_listed()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier.expect_copy().returning(|_, _| Ok(()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().returning(|_, _| Ok(()));
            let mut permissions = target_is_owned_by_dev();
            permissions
                .expect_change_user_and_group_ownership()
                .returning(|_, _, _| Ok(()));
            let mut file_deleter = MockFileDeleter::new();
            file_deleter.expect_delete().times(0);
            let mut folder = MockFolder::new();
            folder
                .expect_entries()
                .returning(|path| Err(FileSystemError::NotFoundError(path.to_path_buf())));
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RetainPreviousBinary::new(version(0, 0, 4));
            let reporter = CapturingReporter::new();
            let span = Span::new(
                Arc::clone(&reporter) as Arc<dyn Reporter>,
                "test",
                ScopeKind::Group,
            );

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("Could not list retained versions"))
            );
        }

        #[tokio::test]
        async fn test_retain_run_should_still_succeed_and_warn_when_an_old_version_cannot_be_removed()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier.expect_copy().returning(|_, _| Ok(()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().returning(|_, _| Ok(()));
            let mut permissions = target_is_owned_by_dev();
            permissions
                .expect_change_user_and_group_ownership()
                .returning(|_, _, _| Ok(()));
            let mut file_deleter = MockFileDeleter::new();
            file_deleter
                .expect_delete()
                .times(1)
                .returning(|path| Err(FileSystemError::NotFoundError(path.to_path_buf())));
            let folder = folder_listing(&[
                "douglas-0.0.1",
                "douglas-0.0.2",
                "douglas-0.0.3",
                "douglas-0.0.4",
            ]);
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RetainPreviousBinary::new(version(0, 0, 4));
            let reporter = CapturingReporter::new();
            let span = Span::new(
                Arc::clone(&reporter) as Arc<dyn Reporter>,
                "test",
                ScopeKind::Group,
            );

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("douglas-0.0.1"))
            );
        }

        #[tokio::test]
        async fn test_retain_run_should_fail_without_copying_when_the_binary_link_cannot_be_followed()
         {
            let os = MockOs::new();
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().times(0);
            let mut file_copier = MockFileCopier::new();
            file_copier.expect_copy().times(0);
            let mut file_deleter = MockFileDeleter::new();
            file_deleter.expect_delete().times(0);
            let folder = MockFolder::new();
            let mut links = MockLinks::new();
            links
                .expect_follow_symbolic()
                .returning(|path| Err(FileSystemError::NotFoundError(path.to_path_buf())));
            let permissions = MockPermissions::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RetainPreviousBinary::new(version(0, 0, 4));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_retain_run_should_remove_the_partial_file_and_fail_when_the_copy_fails() {
            let mut file_copier = MockFileCopier::new();
            file_copier
                .expect_copy()
                .returning(|_, _| Err(permission_denied()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().times(0);
            let mut file_deleter = MockFileDeleter::new();
            file_deleter
                .expect_delete()
                .withf(|path| path == partial())
                .times(1)
                .returning(|_| Ok(()));
            let folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RetainPreviousBinary::new(version(0, 0, 4));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_retain_run_should_remove_the_partial_file_and_fail_when_ownership_cannot_be_set()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier.expect_copy().returning(|_, _| Ok(()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().times(0);
            let mut permissions = target_is_owned_by_dev();
            permissions
                .expect_change_user_and_group_ownership()
                .returning(|_, _, _| Err(permission_denied()));
            let mut file_deleter = MockFileDeleter::new();
            file_deleter
                .expect_delete()
                .withf(|path| path == partial())
                .times(1)
                .returning(|_| Ok(()));
            let folder = MockFolder::new();
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RetainPreviousBinary::new(version(0, 0, 4));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_retain_run_should_remove_the_partial_file_and_fail_when_the_final_rename_fails()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier.expect_copy().returning(|_, _| Ok(()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer
                .expect_rename()
                .returning(|_, _| Err(permission_denied()));
            let mut permissions = target_is_owned_by_dev();
            permissions
                .expect_change_user_and_group_ownership()
                .returning(|_, _, _| Ok(()));
            let mut file_deleter = MockFileDeleter::new();
            file_deleter
                .expect_delete()
                .withf(|path| path == partial())
                .times(1)
                .returning(|_| Ok(()));
            let folder = MockFolder::new();
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RetainPreviousBinary::new(version(0, 0, 4));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_rename_the_candidate_onto_the_links_real_target() {
            let mut file_renamer = MockFileRenamer::new();
            file_renamer
                .expect_rename()
                .withf(|from, to| from == Path::new(CANDIDATE) && to == Path::new(TARGET))
                .times(1)
                .returning(|_, _| Ok(()));
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = OverwriteDouglasExecutable::new(Path::new(CANDIDATE));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_not_rename_when_the_binary_link_cannot_be_followed() {
            let os = MockOs::new();
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().times(0);
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let folder = MockFolder::new();
            let mut links = MockLinks::new();
            links
                .expect_follow_symbolic()
                .returning(|path| Err(FileSystemError::NotFoundError(path.to_path_buf())));
            let permissions = MockPermissions::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = OverwriteDouglasExecutable::new(Path::new(CANDIDATE));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_fail_without_copying_when_the_rename_fails_for_another_reason()
         {
            let mut file_renamer = MockFileRenamer::new();
            file_renamer
                .expect_rename()
                .times(1)
                .returning(|_, _| Err(permission_denied()));
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let folder = MockFolder::new();
            let permissions = MockPermissions::new();
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = OverwriteDouglasExecutable::new(Path::new(CANDIDATE));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_copy_chown_rename_and_remove_the_source_when_crossing_filesystems()
         {
            let mut sequence = Sequence::new();
            let mut file_renamer = MockFileRenamer::new();
            let mut file_copier = MockFileCopier::new();
            let folder = MockFolder::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = MockPermissions::new();
            file_renamer
                .expect_rename()
                .withf(|from, to| from == Path::new(CANDIDATE) && to == Path::new(TARGET))
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_, _| Err(cross_device()));
            file_copier
                .expect_copy()
                .withf(|from, to| from == Path::new(CANDIDATE) && to == Path::new(STAGING))
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_, _| Ok(()));
            permissions
                .expect_get_user_and_group_ownership()
                .withf(|path| path == Path::new(CANDIDATE))
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_| Ok(("root".to_string(), "douglas-admin".to_string())));
            permissions
                .expect_change_user_and_group_ownership()
                .withf(|path, user, group| {
                    path == Path::new(STAGING) && user == "root" && group == "douglas-admin"
                })
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_, _, _| Ok(()));
            file_renamer
                .expect_rename()
                .withf(|from, to| from == Path::new(STAGING) && to == Path::new(TARGET))
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_, _| Ok(()));
            file_deleter
                .expect_delete()
                .withf(|path| path == Path::new(CANDIDATE))
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_| Ok(()));
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = OverwriteDouglasExecutable::new(Path::new(CANDIDATE));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_remove_the_staging_file_and_keep_the_candidate_when_the_copy_fails()
         {
            let mut file_renamer = MockFileRenamer::new();
            let mut file_copier = MockFileCopier::new();
            let folder = MockFolder::new();
            let mut file_deleter = MockFileDeleter::new();
            let permissions = candidate_is_root_owned_by_douglas_admin();
            file_renamer
                .expect_rename()
                .withf(|from, _| from == Path::new(CANDIDATE))
                .times(1)
                .returning(|_, _| Err(cross_device()));
            file_copier
                .expect_copy()
                .times(1)
                .returning(|_, _| Err(permission_denied()));
            file_deleter
                .expect_delete()
                .withf(|path| path == Path::new(STAGING))
                .times(1)
                .returning(|_| Ok(()));
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = OverwriteDouglasExecutable::new(Path::new(CANDIDATE));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_remove_the_staging_file_and_keep_the_candidate_when_ownership_cannot_be_set()
         {
            let mut file_renamer = MockFileRenamer::new();
            let mut file_copier = MockFileCopier::new();
            let folder = MockFolder::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = candidate_is_root_owned_by_douglas_admin();
            file_renamer
                .expect_rename()
                .withf(|from, _| from == Path::new(CANDIDATE))
                .times(1)
                .returning(|_, _| Err(cross_device()));
            file_copier.expect_copy().times(1).returning(|_, _| Ok(()));
            permissions
                .expect_change_user_and_group_ownership()
                .times(1)
                .returning(|_, _, _| Err(permission_denied()));
            file_deleter
                .expect_delete()
                .withf(|path| path == Path::new(STAGING))
                .times(1)
                .returning(|_| Ok(()));
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = OverwriteDouglasExecutable::new(Path::new(CANDIDATE));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_remove_the_staging_file_and_keep_the_candidate_when_the_final_rename_fails()
         {
            let mut file_renamer = MockFileRenamer::new();
            let mut file_copier = MockFileCopier::new();
            let folder = MockFolder::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = candidate_is_root_owned_by_douglas_admin();
            file_renamer
                .expect_rename()
                .withf(|from, _| from == Path::new(CANDIDATE))
                .times(1)
                .returning(|_, _| Err(cross_device()));
            file_copier.expect_copy().times(1).returning(|_, _| Ok(()));
            permissions
                .expect_change_user_and_group_ownership()
                .times(1)
                .returning(|_, _, _| Ok(()));
            file_renamer
                .expect_rename()
                .withf(|from, to| from == Path::new(STAGING) && to == Path::new(TARGET))
                .times(1)
                .returning(|_, _| Err(permission_denied()));
            file_deleter
                .expect_delete()
                .withf(|path| path == Path::new(STAGING))
                .times(1)
                .returning(|_| Ok(()));
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = OverwriteDouglasExecutable::new(Path::new(CANDIDATE));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_still_succeed_and_warn_when_the_candidate_cannot_be_removed()
         {
            let mut file_renamer = MockFileRenamer::new();
            let mut file_copier = MockFileCopier::new();
            let folder = MockFolder::new();
            let mut file_deleter = MockFileDeleter::new();
            let mut permissions = candidate_is_root_owned_by_douglas_admin();
            file_renamer
                .expect_rename()
                .withf(|from, _| from == Path::new(CANDIDATE))
                .times(1)
                .returning(|_, _| Err(cross_device()));
            file_copier.expect_copy().times(1).returning(|_, _| Ok(()));
            permissions
                .expect_change_user_and_group_ownership()
                .times(1)
                .returning(|_, _, _| Ok(()));
            file_renamer
                .expect_rename()
                .withf(|from, to| from == Path::new(STAGING) && to == Path::new(TARGET))
                .times(1)
                .returning(|_, _| Ok(()));
            file_deleter
                .expect_delete()
                .withf(|path| path == Path::new(CANDIDATE))
                .times(1)
                .returning(|_| Err(permission_denied()));
            let os = MockOs::new();
            let links = links_to_target();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = OverwriteDouglasExecutable::new(Path::new(CANDIDATE));
            let reporter = CapturingReporter::new();
            let span = Span::new(
                Arc::clone(&reporter) as Arc<dyn Reporter>,
                "test",
                ScopeKind::Group,
            );

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
            assert!(
                reporter
                    .messages()
                    .iter()
                    .any(|message| message.contains("could not remove"))
            );
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
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
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
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    folder: &folder,
                },
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
