use crate::{
    bootstrap::{
        HasServiceControl, KillService, OwnedServiceControl, ServiceControl, StopBract, journal,
        retention,
    },
    cli::Presentation,
    commands::print_error,
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
    FileCopier, FileDeleter, FileReader, FileRenamer, FileSystemError, FileWriter, Folder, Inspect,
    Links, Permissions, UnixFileCopier, UnixFileDeleter, UnixFileRenamer, UnixFileWriter,
    UnixFolder, UnixInspect, UnixLinks, UnixPermissions, path_to_string,
};
use log::{Level, Outcome, Reporter, ScopeGuard, ScopeKind, Span};
use os::Os;
use release::{Difference, differences};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
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
    #[error(
        "This upgrade cannot be rolled back from ({}); pass --allow-one-way to proceed anyway",
        describe_differences(.0)
    )]
    OneWay(Vec<Difference>),
    #[error("The new version failed to start (exit code {code}): {detail}")]
    StartFailed { code: String, detail: String },
    #[error("Could not run the installed version: {0}")]
    CannotRun(#[from] os::OsError),
    #[error("{service} is not healthy after the upgrade: {reason}")]
    Unhealthy { service: String, reason: String },
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
}

fn describe_differences(found: &[Difference]) -> String {
    found
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

struct FileOperations<'a> {
    renamer: &'a dyn FileRenamer,
    copier: &'a dyn FileCopier,
    deleter: &'a dyn FileDeleter,
    writer: &'a dyn FileWriter,
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
        candidate: Version,
        one_way: Vec<Difference>,
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
            candidate: external.version,
            one_way: differences(&internal.metadata, &external.metadata),
            is_marked_as_executable,
            is_owned_by_douglas_admin,
        }))
    }
}

fn create_plan<'a>(
    state: &State,
    path: &Path,
    presentation: Presentation,
    allow_one_way: bool,
) -> Result<Vec<Step<'a>>, UpgradeError> {
    let (current, candidate, restorable, is_marked_as_executable, is_owned_by_douglas_admin) =
        match state {
            State::NotRoot => return Err(UpgradeError::MustBeRoot),
            State::Missing => return Err(UpgradeError::Missing(path.to_path_buf())),
            State::NotNewer => return Err(UpgradeError::InvalidUpgrade),
            State::Upgradable {
                current,
                candidate,
                one_way,
                is_marked_as_executable,
                is_owned_by_douglas_admin,
            } => {
                if !one_way.is_empty() && !allow_one_way {
                    return Err(UpgradeError::OneWay(one_way.clone()));
                }
                (
                    *current,
                    *candidate,
                    one_way.is_empty(),
                    *is_marked_as_executable,
                    *is_owned_by_douglas_admin,
                )
            }
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
    if restorable {
        push_step(&mut result, RestartPreviousVersion::new(presentation));
    }
    push_step(&mut result, RecordUpgrade::new(current, candidate));
    push_step(&mut result, KillService::new(config::services::WOODWARD));
    push_step(&mut result, StopBract::new(false));
    push_step(&mut result, KillService::new(config::services::RESIN));
    push_step(&mut result, KillService::new(config::services::SEEDBANK));
    push_step(
        &mut result,
        OverwriteDouglasExecutable::new(path, current, restorable),
    );
    push_step(&mut result, StartNewVersion::new(presentation));
    push_step(&mut result, ConfirmHealthy::new());
    push_step(&mut result, ClearUpgradeRecord);

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
    previous: Version,
    restorable: bool,
}

impl OverwriteDouglasExecutable {
    pub fn new(path: &Path, previous: Version, restorable: bool) -> Self {
        Self {
            path: path.to_path_buf(),
            previous,
            restorable,
        }
    }

    fn install_target(context: &Context<'_>) -> Result<PathBuf, FileSystemError> {
        let link = context.douglas_folders.binary_link();
        Ok(context
            .douglas_folders
            .binary_dir()
            .join(context.links.follow_symbolic(&link)?))
    }

    fn restore_previous(&self, context: &Context<'_>) -> Result<(), FileSystemError> {
        let target = Self::install_target(context)?;
        let retained =
            retention::retained_path(&context.douglas_folders.binary_dir(), self.previous);
        let staging = staging_path(&target)?;

        let result = context
            .files
            .copier
            .copy(&retained, &staging)
            .and_then(|()| {
                let (user, group) = context
                    .permissions
                    .get_user_and_group_ownership(&retained)?;
                context
                    .permissions
                    .change_user_and_group_ownership(&staging, &user, &group)
            })
            .and_then(|()| context.files.renamer.rename(&staging, &target));

        if result.is_err() {
            let _ = context.files.deleter.delete(&staging);
        }
        result
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

        let target = Self::install_target(context)?;
        match context.files.renamer.rename(&self.path, &target) {
            Err(err) if err.is_cross_device() => self.copy_into_place(&guard, context, &target)?,
            result => result?,
        }

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }

    async fn rollback(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                "Restoring the previous version of douglas…",
                ScopeKind::Step,
            )
            .start_guard();

        if !self.restorable {
            guard.span().message(
                Level::Warn,
                &format!(
                    "Leaving the new version installed: this upgrade cannot be rolled back, \
                     so version {} is not restored",
                    self.previous
                ),
            );
            guard.finish_with_outcome(Outcome::Ok);
            return Ok(());
        }

        self.restore_previous(context)?;

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

fn start_args(presentation: Presentation) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(value) = presentation.output_style().to_possible_value() {
        args.push("--output-style".to_string());
        args.push(value.get_name().to_string());
    }
    args.push("start".to_string());
    args
}

const FAILURE_DETAIL_LINES: usize = 5;

fn failure_detail(stdout: &str, stderr: &str) -> String {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = &lines[lines.len().saturating_sub(FAILURE_DETAIL_LINES)..];
    if tail.is_empty() {
        "no output".to_string()
    } else {
        tail.join(" | ")
    }
}

fn relay(presentation: Presentation, stdout: &str, stderr: &str) {
    if presentation == Presentation::Interactive {
        return;
    }
    print!("{stdout}");
    eprint!("{stderr}");
}

fn start_installed_version(
    context: &Context<'_>,
    presentation: Presentation,
) -> Result<(), UpgradeError> {
    let result = context.service_control.os.execute_with_output(
        &path_to_string(context.douglas_folders.binary_link()),
        start_args(presentation),
        vec![],
    );

    match result {
        Ok(output) => {
            relay(
                presentation,
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr),
            );
            Ok(())
        }
        Err(os::OsError::ProccessExitStatusError {
            code,
            stdout,
            stderr,
            ..
        }) => {
            relay(presentation, &stdout, &stderr);
            Err(UpgradeError::StartFailed {
                code: code.map_or_else(|| "none".to_string(), |value| value.to_string()),
                detail: failure_detail(&stdout, &stderr),
            })
        }
        Err(err) => Err(err.into()),
    }
}

#[derive(Debug)]
struct StartNewVersion {
    presentation: Presentation,
}

impl StartNewVersion {
    pub fn new(presentation: Presentation) -> Self {
        Self { presentation }
    }

    async fn stop_what_the_new_version_started<'a>(span: &Span, context: &mut Context<'a>) {
        let mut steps: Vec<Step<'a>> = Vec::new();
        push_step(&mut steps, KillService::new(config::services::WOODWARD));
        push_step(&mut steps, StopBract::new(false));
        push_step(&mut steps, KillService::new(config::services::RESIN));
        push_step(&mut steps, KillService::new(config::services::SEEDBANK));

        for step in &mut steps {
            if let Err(err) = step.run(span, context).await {
                span.message(Level::Info, &format!("[{step}] {err}"));
            }
        }
    }
}

impl std::fmt::Display for StartNewVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Start the new version")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for StartNewVersion {
    fn name(&self) -> String {
        "Start the new version".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Starting the new version…", ScopeKind::Step)
            .start_guard();

        if let Err(err) = start_installed_version(context, self.presentation) {
            Self::stop_what_the_new_version_started(guard.span(), context).await;
            return guard.finish(Err(err.into()));
        }

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }

    async fn rollback(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Stopping the new version…", ScopeKind::Step)
            .start_guard();

        Self::stop_what_the_new_version_started(guard.span(), context).await;

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

const HEALTH_SOAK: Duration = Duration::from_secs(10);
const HEALTH_INTERVAL: Duration = Duration::from_secs(1);
const HEALTH_SERVICES: [&str; 4] = [
    config::services::WOODWARD,
    config::services::BRACT,
    config::services::RESIN,
    config::services::SEEDBANK,
];

#[derive(Debug)]
struct ConfirmHealthy {
    soak: Duration,
    interval: Duration,
}

impl ConfirmHealthy {
    pub fn new() -> Self {
        Self::with_soak(HEALTH_SOAK, HEALTH_INTERVAL)
    }

    fn with_soak(soak: Duration, interval: Duration) -> Self {
        Self { soak, interval }
    }

    fn polls(&self) -> u128 {
        self.soak.as_millis() / self.interval.as_millis().max(1)
    }

    fn check_once(
        context: &Context<'_>,
        seen: &mut HashMap<&'static str, u32>,
    ) -> Result<(), UpgradeError> {
        let control = &context.service_control;

        for service in HEALTH_SERVICES {
            let unhealthy = |reason: String| UpgradeError::Unhealthy {
                service: service.to_string(),
                reason,
            };

            let pid = control
                .heartbeat_reader_factory
                .create(service)
                .read()
                .map_err(|err| unhealthy(format!("its heartbeat could not be read: {err}")))?
                .pid;

            let alive = control
                .os
                .is_active_pid(pid)
                .map_err(|err| unhealthy(format!("its process could not be checked: {err}")))?;
            if !alive {
                return Err(unhealthy(format!("process {pid} is not running")));
            }

            match seen.insert(service, pid) {
                Some(earlier) if earlier != pid => {
                    return Err(unhealthy(format!("restarted (process {earlier} -> {pid})")));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl std::fmt::Display for ConfirmHealthy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Confirm the new version is healthy")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for ConfirmHealthy {
    fn name(&self) -> String {
        "Confirm the new version is healthy".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Confirming the new version is healthy…", ScopeKind::Step)
            .start_guard();

        let mut seen = HashMap::new();
        for poll in 0..=self.polls() {
            if poll > 0 {
                context.service_control.os.sleep(self.interval);
            }
            if let Err(err) = Self::check_once(context, &mut seen) {
                return guard.finish(Err(err.into()));
            }
        }

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

#[derive(Debug)]
struct RecordUpgrade {
    from: Version,
    to: Version,
}

impl RecordUpgrade {
    pub fn new(from: Version, to: Version) -> Self {
        Self { from, to }
    }
}

impl std::fmt::Display for RecordUpgrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Record the upgrade in progress")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for RecordUpgrade {
    fn name(&self) -> String {
        "Record the upgrade in progress".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Recording the upgrade in progress…", ScopeKind::Step)
            .start_guard();

        let entry = journal::UpgradeJournal {
            from: self.from.to_string(),
            to: self.to.to_string(),
            previous_binary: retention::retained_path(
                &context.douglas_folders.binary_dir(),
                self.from,
            ),
            pid: context.service_control.os.current_pid(),
        };
        if let Err(err) = journal::record(&context.douglas_folders, context.files.writer, &entry) {
            return guard.finish(Err(err.into()));
        }

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

#[derive(Debug)]
struct ClearUpgradeRecord;

impl ClearUpgradeRecord {
    fn clear(guard: &ScopeGuard, context: &Context<'_>) {
        if let Err(err) = journal::clear(&context.douglas_folders, context.files.deleter) {
            guard.span().message(Level::Warn, &err.to_string());
        }
    }
}

impl std::fmt::Display for ClearUpgradeRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clear the upgrade record")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for ClearUpgradeRecord {
    fn name(&self) -> String {
        "Clear the upgrade record".to_string()
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Clearing the upgrade record…", ScopeKind::Step)
            .start_guard();

        Self::clear(&guard, context);

        guard.finish_with_outcome(Outcome::Ok);
        Ok(())
    }
}

#[derive(Debug)]
struct RestartPreviousVersion {
    presentation: Presentation,
}

impl RestartPreviousVersion {
    pub fn new(presentation: Presentation) -> Self {
        Self { presentation }
    }
}

impl std::fmt::Display for RestartPreviousVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Bring the previous version back up if the upgrade fails")
    }
}

#[async_trait]
impl<'a> Command<Context<'a>> for RestartPreviousVersion {
    fn name(&self) -> String {
        "Bring the previous version back up if the upgrade fails".to_string()
    }

    async fn run(
        &mut self,
        _span: &Span,
        _context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    async fn rollback(
        &mut self,
        span: &Span,
        context: &mut Context<'a>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child("Starting the previous version again…", ScopeKind::Step)
            .start_guard();

        if let Err(err) = start_installed_version(context, self.presentation) {
            return guard.finish(Err(err.into()));
        }

        ClearUpgradeRecord::clear(&guard, context);

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
    allow_one_way: bool,
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

    if allow_one_way
        && let State::Upgradable { one_way, .. } = &state
        && !one_way.is_empty()
    {
        guard.span().message(
            Level::Warn,
            &format!(
                "This upgrade cannot be rolled back from ({})",
                describe_differences(one_way)
            ),
        );
    }

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
    let file_writer = UnixFileWriter::new();
    let folder = UnixFolder::new();
    let links = UnixLinks::new();

    let plan = match resolve_plan(
        guard.span(),
        create_plan(&state, path, presentation, allow_one_way),
    ) {
        Ok(plan) => plan,
        Err(err) => {
            guard.span().message(Level::Warn, &err.to_string());
            guard.finish_with_outcome(log::Outcome::Failed);
            if let Some(style) = presentation.console_style() {
                print_error(style, &err.to_string());
            }
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
            writer: &file_writer,
            folder: &folder,
        },
        links: &links,
        permissions: &permissions,
    };

    let result = execute_plan(guard.span(), plan, &mut context, |reason| reason).await;

    match result {
        Ok(()) => {
            guard.finish_with_outcome(Outcome::Ok);
            true
        }
        Err(reason) => {
            guard.finish_with_outcome(Outcome::Failed);
            if let Some(style) = presentation.console_style() {
                print_error(style, &reason);
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{MockBinaryVerifier, Release, Version};
    use credentials::MockCredentials;
    use file_system::{
        MockFileCopier, MockFileDeleter, MockFileRenamer, MockFileWriter, MockFolder, MockInspect,
        MockLinks, MockPermissions,
    };
    use heartbeat::HeartbeatReaderFactory;
    use mockall::Sequence;
    use os::MockOs;
    use release::ReleaseMetadata;
    use std::os::unix::process::ExitStatusExt;
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

            let result = create_plan(&state, &candidate_path(), Presentation::Plain, false);

            assert!(matches!(result, Err(UpgradeError::MustBeRoot)));
        }

        #[test]
        fn test_should_error_when_binary_does_not_exist() {
            let state = State::Missing;

            let result = create_plan(&state, &candidate_path(), Presentation::Plain, false);

            assert!(matches!(result, Err(UpgradeError::Missing(path)) if path == candidate_path()));
        }

        #[test]
        fn test_should_error_when_not_a_higher_version() {
            let state = State::NotNewer;

            let result = create_plan(&state, &candidate_path(), Presentation::Plain, false);

            assert!(matches!(result, Err(UpgradeError::InvalidUpgrade)));
        }

        #[test]
        fn test_should_skip_mark_executable_and_set_ownership_when_already_correct() {
            let state = State::Upgradable {
                current: version(0, 0, 1),
                candidate: version(9, 9, 9),
                one_way: Vec::new(),
                is_marked_as_executable: true,
                is_owned_by_douglas_admin: true,
            };

            let Ok(steps) = create_plan(&state, &candidate_path(), Presentation::Plain, false)
            else {
                panic!("should plan");
            };
            let descriptions: Vec<String> =
                steps.iter().map(std::string::ToString::to_string).collect();

            assert_eq!(
                descriptions,
                vec![
                    "Keep the current version for rollback".to_string(),
                    "Bring the previous version back up if the upgrade fails".to_string(),
                    "Record the upgrade in progress".to_string(),
                    "Kill service woodward".to_string(),
                    "Stopping Bract".to_string(),
                    "Kill service resin".to_string(),
                    "Kill service seedbank".to_string(),
                    "Overwrite current version with new version".to_string(),
                    "Start the new version".to_string(),
                    "Confirm the new version is healthy".to_string(),
                    "Clear the upgrade record".to_string(),
                ]
            );
        }

        #[test]
        fn test_should_mark_executable_and_set_ownership_first_when_needed() {
            let state = State::Upgradable {
                current: version(0, 0, 1),
                candidate: version(9, 9, 9),
                one_way: Vec::new(),
                is_marked_as_executable: false,
                is_owned_by_douglas_admin: false,
            };

            let Ok(steps) = create_plan(&state, &candidate_path(), Presentation::Plain, false)
            else {
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
                    "Bring the previous version back up if the upgrade fails".to_string(),
                    "Record the upgrade in progress".to_string(),
                    "Kill service woodward".to_string(),
                    "Stopping Bract".to_string(),
                    "Kill service resin".to_string(),
                    "Kill service seedbank".to_string(),
                    "Overwrite current version with new version".to_string(),
                    "Start the new version".to_string(),
                    "Confirm the new version is healthy".to_string(),
                    "Clear the upgrade record".to_string(),
                ]
            );
        }
        fn one_way_state() -> State {
            State::Upgradable {
                current: version(0, 0, 1),
                candidate: version(9, 9, 9),
                one_way: vec![Difference::CoreVersion {
                    seedling: "openbao".to_string(),
                    from: 1,
                    to: 2,
                }],
                is_marked_as_executable: true,
                is_owned_by_douglas_admin: true,
            }
        }

        #[test]
        fn test_should_refuse_a_one_way_upgrade_and_say_what_changed() {
            let result = create_plan(
                &one_way_state(),
                &candidate_path(),
                Presentation::Plain,
                false,
            );

            let Err(err) = result else {
                panic!("should refuse");
            };
            assert!(matches!(err, UpgradeError::OneWay(_)));
            let message = err.to_string();
            assert!(message.contains("core seedling 'openbao' version 1 -> 2"));
            assert!(message.contains("--allow-one-way"));
        }

        #[test]
        fn test_should_plan_a_one_way_upgrade_without_a_way_back_when_allowed() {
            let Ok(steps) = create_plan(
                &one_way_state(),
                &candidate_path(),
                Presentation::Plain,
                true,
            ) else {
                panic!("should plan");
            };
            let descriptions: Vec<String> =
                steps.iter().map(std::string::ToString::to_string).collect();

            assert!(
                !descriptions
                    .iter()
                    .any(|description| description.contains("previous version back up"))
            );
            assert!(descriptions.contains(&"Start the new version".to_string()));
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
        fn test_should_start_plain_when_interactive_because_the_terminal_is_not_the_childs() {
            assert_eq!(
                start_args(Presentation::Interactive),
                vec!["--output-style", "plain", "start"]
            );
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

    mod failure_detail_tests {
        use super::*;

        #[test]
        fn test_should_prefer_what_the_process_wrote_to_stderr() {
            assert_eq!(failure_detail("fine", "broken"), "broken");
        }

        #[test]
        fn test_should_fall_back_to_stdout_when_stderr_is_empty() {
            assert_eq!(failure_detail("only stdout", "  \n"), "only stdout");
        }

        #[test]
        fn test_should_keep_only_the_last_five_non_blank_lines() {
            let stderr = "one\ntwo\n\nthree\nfour\nfive\nsix\nseven\n";

            assert_eq!(
                failure_detail("", stderr),
                "three | four | five | six | seven"
            );
        }

        #[test]
        fn test_should_say_so_when_there_was_no_output() {
            assert_eq!(failure_detail("", ""), "no output");
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
                    ..
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
                    ..
                }
            ));
        }
        #[test]
        fn test_should_report_no_one_way_differences_when_the_metadata_matches() {
            let mut credentials = MockCredentials::new();
            credentials.expect_is_root().returning(|| true);
            let mut inspect = MockInspect::new();
            inspect.expect_exists().returning(|_| true);
            let mut binary_verifier = MockBinaryVerifier::new();
            binary_verifier
                .expect_get_external_release()
                .returning(|_| {
                    let candidate = release(2, 0, 0);

                    Ok(candidate)
                });
            binary_verifier
                .expect_get_internal_release()
                .returning(|| Ok(release(1, 0, 0)));
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_mode()
                .returning(|_| Ok(file_system::Modes::OwnerReadWriteExecute));
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

            let Ok(State::Upgradable { one_way, .. }) =
                observer.discover(guard.span(), Path::new("/tmp/candidate"))
            else {
                panic!("should discover an upgradable state");
            };

            assert!(one_way.is_empty());
        }

        #[test]
        fn test_should_report_a_one_way_difference_when_a_core_seedling_version_changes() {
            let mut credentials = MockCredentials::new();
            credentials.expect_is_root().returning(|| true);
            let mut inspect = MockInspect::new();
            inspect.expect_exists().returning(|_| true);
            let mut binary_verifier = MockBinaryVerifier::new();
            binary_verifier
                .expect_get_external_release()
                .returning(|_| {
                    let mut candidate = release(2, 0, 0);
                    candidate.metadata.core.insert("openbao".to_string(), 99);
                    Ok(candidate)
                });
            binary_verifier
                .expect_get_internal_release()
                .returning(|| Ok(release(1, 0, 0)));
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_mode()
                .returning(|_| Ok(file_system::Modes::OwnerReadWriteExecute));
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

            let Ok(State::Upgradable { one_way, .. }) =
                observer.discover(guard.span(), Path::new("/tmp/candidate"))
            else {
                panic!("should discover an upgradable state");
            };

            assert_eq!(
                one_way,
                vec![Difference::CoreVersion {
                    seedling: "openbao".to_string(),
                    from: config::seedlings::OPENBAO_VERSION,
                    to: 99,
                }]
            );
        }

        #[test]
        fn test_should_report_a_one_way_difference_when_the_data_format_changes() {
            let mut credentials = MockCredentials::new();
            credentials.expect_is_root().returning(|| true);
            let mut inspect = MockInspect::new();
            inspect.expect_exists().returning(|_| true);
            let mut binary_verifier = MockBinaryVerifier::new();
            binary_verifier
                .expect_get_external_release()
                .returning(|_| {
                    let mut candidate = release(2, 0, 0);
                    candidate.metadata.format = 99;
                    Ok(candidate)
                });
            binary_verifier
                .expect_get_internal_release()
                .returning(|| Ok(release(1, 0, 0)));
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_mode()
                .returning(|_| Ok(file_system::Modes::OwnerReadWriteExecute));
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

            let Ok(State::Upgradable { one_way, .. }) =
                observer.discover(guard.span(), Path::new("/tmp/candidate"))
            else {
                panic!("should discover an upgradable state");
            };

            assert_eq!(
                one_way,
                vec![Difference::Format {
                    from: config::DATA_FORMAT,
                    to: 99,
                }]
            );
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
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
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
            let file_writer = MockFileWriter::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
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
            let file_writer = MockFileWriter::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
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
            let file_writer = MockFileWriter::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
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
            let file_writer = MockFileWriter::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
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
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
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
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
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
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
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
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
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
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
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
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
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
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
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
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_remove_the_staging_file_and_keep_the_candidate_when_the_copy_fails()
         {
            let mut file_renamer = MockFileRenamer::new();
            let mut file_copier = MockFileCopier::new();
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_remove_the_staging_file_and_keep_the_candidate_when_ownership_cannot_be_set()
         {
            let mut file_renamer = MockFileRenamer::new();
            let mut file_copier = MockFileCopier::new();
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_remove_the_staging_file_and_keep_the_candidate_when_the_final_rename_fails()
         {
            let mut file_renamer = MockFileRenamer::new();
            let mut file_copier = MockFileCopier::new();
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_run_should_still_succeed_and_warn_when_the_candidate_cannot_be_removed()
         {
            let mut file_renamer = MockFileRenamer::new();
            let mut file_copier = MockFileCopier::new();
            let file_writer = MockFileWriter::new();
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
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
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
        async fn test_overwrite_rollback_should_copy_the_retained_version_over_the_target_with_its_owner()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier
                .expect_copy()
                .withf(|from, to| from == retained() && to == Path::new(STAGING))
                .times(1)
                .returning(|_, _| Ok(()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer
                .expect_rename()
                .withf(|from, to| from == Path::new(STAGING) && to == Path::new(TARGET))
                .times(1)
                .returning(|_, _| Ok(()));
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_user_and_group_ownership()
                .withf(|path| path == retained())
                .returning(|_| Ok(("root".to_string(), "douglas-admin".to_string())));
            permissions
                .expect_change_user_and_group_ownership()
                .withf(|path, user, group| {
                    path == Path::new(STAGING) && user == "root" && group == "douglas-admin"
                })
                .times(1)
                .returning(|_, _, _| Ok(()));
            let mut file_deleter = MockFileDeleter::new();
            file_deleter.expect_delete().times(0);
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let links = links_to_target();
            let os = MockOs::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_overwrite_rollback_should_leave_the_new_version_and_warn_when_the_upgrade_is_one_way()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier.expect_copy().times(0);
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().times(0);
            let permissions = MockPermissions::new();
            let mut file_deleter = MockFileDeleter::new();
            file_deleter.expect_delete().times(0);
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let links = MockLinks::new();
            let os = MockOs::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), false);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_overwrite_rollback_should_fail_without_copying_when_the_binary_link_cannot_be_followed()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier.expect_copy().times(0);
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().times(0);
            let permissions = MockPermissions::new();
            let mut file_deleter = MockFileDeleter::new();
            file_deleter.expect_delete().times(0);
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let links = {
                let mut links = MockLinks::new();
                links
                    .expect_follow_symbolic()
                    .returning(|path| Err(FileSystemError::NotFoundError(path.to_path_buf())));
                links
            };
            let os = MockOs::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_rollback_should_remove_the_staging_file_and_fail_when_the_copy_fails()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier
                .expect_copy()
                .returning(|_, _| Err(permission_denied()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().times(0);
            let permissions = MockPermissions::new();
            let mut file_deleter = MockFileDeleter::new();
            file_deleter
                .expect_delete()
                .withf(|path| path == Path::new(STAGING))
                .times(1)
                .returning(|_| Ok(()));
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let links = links_to_target();
            let os = MockOs::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_rollback_should_remove_the_staging_file_and_fail_when_ownership_cannot_be_set()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier.expect_copy().returning(|_, _| Ok(()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer.expect_rename().times(0);
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_user_and_group_ownership()
                .withf(|path| path == retained())
                .returning(|_| Ok(("root".to_string(), "douglas-admin".to_string())));
            permissions
                .expect_change_user_and_group_ownership()
                .returning(|_, _, _| Err(permission_denied()));
            let mut file_deleter = MockFileDeleter::new();
            file_deleter
                .expect_delete()
                .withf(|path| path == Path::new(STAGING))
                .times(1)
                .returning(|_| Ok(()));
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let links = links_to_target();
            let os = MockOs::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_overwrite_rollback_should_remove_the_staging_file_and_fail_when_the_final_rename_fails()
         {
            let mut file_copier = MockFileCopier::new();
            file_copier.expect_copy().returning(|_, _| Ok(()));
            let mut file_renamer = MockFileRenamer::new();
            file_renamer
                .expect_rename()
                .returning(|_, _| Err(permission_denied()));
            let mut permissions = MockPermissions::new();
            permissions
                .expect_get_user_and_group_ownership()
                .withf(|path| path == retained())
                .returning(|_| Ok(("root".to_string(), "douglas-admin".to_string())));
            permissions
                .expect_change_user_and_group_ownership()
                .returning(|_, _, _| Ok(()));
            let mut file_deleter = MockFileDeleter::new();
            file_deleter
                .expect_delete()
                .withf(|path| path == Path::new(STAGING))
                .times(1)
                .returning(|_| Ok(()));
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let links = links_to_target();
            let os = MockOs::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                OverwriteDouglasExecutable::new(Path::new(CANDIDATE), version(0, 0, 4), true);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_start_new_version_run_should_start_plain_from_the_binary_link_and_succeed() {
            let mut os = MockOs::new();
            let expected_path = path_to_string(DouglasFolders::default().binary_link());
            os.expect_execute_with_output()
                .withf(move |command, args, _env| {
                    command == expected_path
                        && *args
                            == vec![
                                "--output-style".to_string(),
                                "plain".to_string(),
                                "start".to_string(),
                            ]
                })
                .times(1)
                .returning(|_, _, _| {
                    Ok(std::process::Output {
                        status: std::process::ExitStatus::from_raw(0),
                        stdout: b"".to_vec(),
                        stderr: b"".to_vec(),
                    })
                });
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = StartNewVersion::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        fn heartbeat_reader_with_pid(pid: u32) -> Box<dyn heartbeat::HeartbeatReader> {
            let mut reader = heartbeat::MockHeartbeatReader::new();
            reader.expect_read().returning(move || {
                Ok(heartbeat::Heartbeat {
                    pid,
                    written_at: std::time::SystemTime::now(),
                })
            });
            Box::new(reader)
        }

        #[tokio::test]
        async fn test_start_new_version_run_should_stop_what_it_started_and_report_why_when_start_fails()
         {
            let mut os = MockOs::new();
            let expected_path = path_to_string(DouglasFolders::default().binary_link());
            os.expect_execute_with_output()
                .withf(move |command, args, _env| {
                    command == expected_path
                        && *args
                            == vec![
                                "--output-style".to_string(),
                                "plain".to_string(),
                                "start".to_string(),
                            ]
                })
                .times(1)
                .returning(|_, _, _| {
                    Err(os::OsError::ProccessExitStatusError {
                        name: "douglas".to_string(),
                        code: Some(1),
                        args: Vec::new(),
                        stdout: String::new(),
                        stderr: "port 443 already in use".to_string(),
                    })
                });
            os.expect_kill()
                .withf(|pid| *pid == 11)
                .times(1)
                .returning(|_| Ok(()));
            os.expect_kill()
                .withf(|pid| *pid == 33)
                .times(1)
                .returning(|_| Ok(()));
            os.expect_kill()
                .withf(|pid| *pid == 44)
                .times(1)
                .returning(|_| Ok(()));
            let mut heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            for (service, pid) in [
                (config::services::WOODWARD, 11),
                (config::services::RESIN, 33),
                (config::services::SEEDBANK, 44),
            ] {
                heartbeat_reader_factory
                    .expect_create()
                    .withf(move |name| name == service)
                    .times(1)
                    .returning(move |_| heartbeat_reader_with_pid(pid));
            }
            let mut bract_client = bract_client::MockClient::new();
            bract_client
                .expect_stop_bract()
                .withf(|including_containers| !*including_containers)
                .times(1)
                .returning(|_| Ok(()));
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = StartNewVersion::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            let Err(err) = result else {
                panic!("should fail");
            };
            let message = err.to_string();
            assert!(message.contains("exit code 1"));
            assert!(message.contains("port 443 already in use"));
        }

        #[tokio::test]
        async fn test_start_new_version_run_should_still_fail_when_stopping_what_it_started_also_fails()
         {
            let mut os = MockOs::new();
            let expected_path = path_to_string(DouglasFolders::default().binary_link());
            os.expect_execute_with_output()
                .withf(move |command, args, _env| {
                    command == expected_path
                        && *args
                            == vec![
                                "--output-style".to_string(),
                                "plain".to_string(),
                                "start".to_string(),
                            ]
                })
                .times(1)
                .returning(|_, _, _| {
                    Err(os::OsError::ProccessExitStatusError {
                        name: "douglas".to_string(),
                        code: Some(1),
                        args: Vec::new(),
                        stdout: String::new(),
                        stderr: "boom".to_string(),
                    })
                });
            let mut heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            heartbeat_reader_factory.expect_create().returning(|_| {
                let mut reader = heartbeat::MockHeartbeatReader::new();
                reader.expect_read().returning(|| {
                    Err(heartbeat::HeartbeatReaderError::FileSystemError(
                        FileSystemError::NotFoundError(PathBuf::from("/heartbeat")),
                    ))
                });
                Box::new(reader)
            });
            let mut bract_client = bract_client::MockClient::new();
            bract_client
                .expect_stop_bract()
                .returning(|_| Err(bract_client::Error::ConnectionRefused));
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = StartNewVersion::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_start_new_version_run_should_fail_when_the_binary_cannot_be_run() {
            let mut os = MockOs::new();
            os.expect_execute_with_output()
                .returning(|_, _, _| Err(os::OsError::PidTooLarge));
            let mut heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            heartbeat_reader_factory.expect_create().returning(|_| {
                let mut reader = heartbeat::MockHeartbeatReader::new();
                reader.expect_read().returning(|| {
                    Err(heartbeat::HeartbeatReaderError::FileSystemError(
                        FileSystemError::NotFoundError(PathBuf::from("/heartbeat")),
                    ))
                });
                Box::new(reader)
            });
            let mut bract_client = bract_client::MockClient::new();
            bract_client
                .expect_stop_bract()
                .returning(|_| Err(bract_client::Error::ConnectionRefused));
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = StartNewVersion::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_restart_previous_version_rollback_should_say_none_when_the_process_was_killed_by_a_signal()
         {
            let mut os = MockOs::new();
            os.expect_execute_with_output().returning(|_, _, _| {
                Err(os::OsError::ProccessExitStatusError {
                    name: "douglas".to_string(),
                    code: None,
                    args: Vec::new(),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            });
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RestartPreviousVersion::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            let Err(err) = result else {
                panic!("should fail");
            };
            assert!(err.to_string().contains("exit code none"));
        }

        #[tokio::test]
        async fn test_confirm_healthy_run_should_pass_when_every_service_stays_up_through_the_soak()
        {
            let mut os = MockOs::new();
            os.expect_is_active_pid().returning(|_| Ok(true));
            os.expect_sleep()
                .withf(|duration| *duration == Duration::from_secs(1))
                .times(3)
                .returning(|_| ());
            let mut heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            for (service, pid) in [
                (config::services::WOODWARD, 11),
                (config::services::BRACT, 22),
                (config::services::RESIN, 33),
                (config::services::SEEDBANK, 44),
            ] {
                heartbeat_reader_factory
                    .expect_create()
                    .withf(move |name| name == service)
                    .returning(move |_| heartbeat_reader_with_pid(pid));
            }
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                ConfirmHealthy::with_soak(Duration::from_secs(3), Duration::from_secs(1));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_confirm_healthy_run_should_fail_naming_the_service_whose_process_is_gone() {
            let mut os = MockOs::new();
            os.expect_is_active_pid().returning(|pid| Ok(pid != 33));
            os.expect_sleep().times(0);
            let mut heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            for (service, pid) in [
                (config::services::WOODWARD, 11),
                (config::services::BRACT, 22),
                (config::services::RESIN, 33),
                (config::services::SEEDBANK, 44),
            ] {
                heartbeat_reader_factory
                    .expect_create()
                    .withf(move |name| name == service)
                    .returning(move |_| heartbeat_reader_with_pid(pid));
            }
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                ConfirmHealthy::with_soak(Duration::from_secs(3), Duration::from_secs(1));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            let Err(err) = result else {
                panic!("should fail");
            };
            let message = err.to_string();
            assert!(message.contains(config::services::RESIN));
            assert!(message.contains("33"));
        }

        #[tokio::test]
        async fn test_confirm_healthy_run_should_fail_when_a_service_dies_partway_through_the_soak()
        {
            let mut os = MockOs::new();
            let checks = std::sync::atomic::AtomicU32::new(0);
            os.expect_is_active_pid().returning(move |pid| {
                let seen = checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(!(pid == 44 && seen >= 8))
            });
            os.expect_sleep().returning(|_| ());
            let mut heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            for (service, pid) in [
                (config::services::WOODWARD, 11),
                (config::services::BRACT, 22),
                (config::services::RESIN, 33),
                (config::services::SEEDBANK, 44),
            ] {
                heartbeat_reader_factory
                    .expect_create()
                    .withf(move |name| name == service)
                    .returning(move |_| heartbeat_reader_with_pid(pid));
            }
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                ConfirmHealthy::with_soak(Duration::from_secs(3), Duration::from_secs(1));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            let Err(err) = result else {
                panic!("should fail");
            };
            assert!(err.to_string().contains(config::services::SEEDBANK));
        }

        #[tokio::test]
        async fn test_confirm_healthy_run_should_fail_when_a_service_restarts_under_a_new_pid() {
            let mut os = MockOs::new();
            os.expect_is_active_pid().returning(|_| Ok(true));
            os.expect_sleep().returning(|_| ());
            let reads = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let mut heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            heartbeat_reader_factory
                .expect_create()
                .returning(move |service| {
                    if service == config::services::WOODWARD {
                        let reads = Arc::clone(&reads);
                        let mut reader = heartbeat::MockHeartbeatReader::new();
                        reader.expect_read().returning(move || {
                            let count = reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            Ok(heartbeat::Heartbeat {
                                pid: 100 + count,
                                written_at: std::time::SystemTime::now(),
                            })
                        });
                        Box::new(reader)
                    } else {
                        heartbeat_reader_with_pid(7)
                    }
                });
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                ConfirmHealthy::with_soak(Duration::from_secs(3), Duration::from_secs(1));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            let Err(err) = result else {
                panic!("should fail");
            };
            let message = err.to_string();
            assert!(message.contains(config::services::WOODWARD));
            assert!(message.contains("restarted"));
        }

        #[tokio::test]
        async fn test_confirm_healthy_run_should_fail_when_a_heartbeat_cannot_be_read() {
            let mut os = MockOs::new();
            os.expect_is_active_pid().returning(|_| Ok(true));
            let mut heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            heartbeat_reader_factory.expect_create().returning(|_| {
                let mut reader = heartbeat::MockHeartbeatReader::new();
                reader.expect_read().returning(|| {
                    Err(heartbeat::HeartbeatReaderError::FileSystemError(
                        FileSystemError::NotFoundError(PathBuf::from("/heartbeat")),
                    ))
                });
                Box::new(reader)
            });
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                ConfirmHealthy::with_soak(Duration::from_secs(3), Duration::from_secs(1));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            let Err(err) = result else {
                panic!("should fail");
            };
            assert!(err.to_string().contains("heartbeat could not be read"));
        }

        #[tokio::test]
        async fn test_confirm_healthy_run_should_fail_when_the_process_check_itself_fails() {
            let mut os = MockOs::new();
            os.expect_is_active_pid()
                .returning(|_| Err(os::OsError::PidTooLarge));
            let mut heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            for (service, pid) in [
                (config::services::WOODWARD, 11),
                (config::services::BRACT, 22),
                (config::services::RESIN, 33),
                (config::services::SEEDBANK, 44),
            ] {
                heartbeat_reader_factory
                    .expect_create()
                    .withf(move |name| name == service)
                    .returning(move |_| heartbeat_reader_with_pid(pid));
            }
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let bract_client = bract_client::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command =
                ConfirmHealthy::with_soak(Duration::from_secs(3), Duration::from_secs(1));
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            let Err(err) = result else {
                panic!("should fail");
            };
            assert!(err.to_string().contains("could not be checked"));
        }

        #[test]
        fn test_confirm_healthy_polls_should_divide_the_soak_by_the_interval() {
            let command =
                ConfirmHealthy::with_soak(Duration::from_secs(10), Duration::from_secs(2));

            assert_eq!(command.polls(), 5);
        }

        #[test]
        fn test_confirm_healthy_polls_should_not_divide_by_a_zero_interval() {
            let command = ConfirmHealthy::with_soak(Duration::from_secs(1), Duration::ZERO);

            assert_eq!(command.polls(), 1000);
        }

        #[tokio::test]
        async fn test_start_new_version_rollback_should_stop_what_the_new_version_started() {
            let mut os = MockOs::new();
            for pid in [11, 33, 44] {
                os.expect_kill()
                    .withf(move |given| *given == pid)
                    .times(1)
                    .returning(|_| Ok(()));
            }
            let mut heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            for (service, pid) in [
                (config::services::WOODWARD, 11),
                (config::services::RESIN, 33),
                (config::services::SEEDBANK, 44),
            ] {
                heartbeat_reader_factory
                    .expect_create()
                    .withf(move |name| name == service)
                    .times(1)
                    .returning(move |_| heartbeat_reader_with_pid(pid));
            }
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let mut bract_client = bract_client::MockClient::new();
            bract_client
                .expect_stop_bract()
                .withf(|including_containers| !*including_containers)
                .times(1)
                .returning(|_| Ok(()));
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = StartNewVersion::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_record_upgrade_run_should_write_the_journal_with_both_versions_the_retained_binary_and_the_pid()
         {
            let mut os = MockOs::new();
            os.expect_current_pid().returning(|| 4242);
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let mut file_deleter = MockFileDeleter::new();
            file_deleter.expect_delete().times(0);
            let mut file_writer = MockFileWriter::new();
            file_writer
                .expect_write_all()
                .withf(|path, contents| {
                    let expected = journal::UpgradeJournal {
                        from: "0.0.1".to_string(),
                        to: "0.0.2".to_string(),
                        previous_binary: DouglasFolders::default()
                            .binary_dir()
                            .join("douglas-0.0.1"),
                        pid: 4242,
                    };
                    path == DouglasFolders::default().upgrade_journal()
                        && serde_json::from_str::<journal::UpgradeJournal>(contents).ok()
                            == Some(expected)
                })
                .times(1)
                .returning(|_, _| Ok(()));
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RecordUpgrade::new(version(0, 0, 1), version(0, 0, 2));

            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_record_upgrade_run_should_fail_when_the_journal_cannot_be_written() {
            let mut os = MockOs::new();
            os.expect_current_pid().returning(|| 4242);
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let mut file_deleter = MockFileDeleter::new();
            file_deleter.expect_delete().times(0);
            let mut file_writer = MockFileWriter::new();
            file_writer.expect_write_all().returning(|path, _| {
                Err(FileSystemError::IoErrorAtPath {
                    path: path.to_path_buf(),
                    error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                })
            });
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RecordUpgrade::new(version(0, 0, 1), version(0, 0, 2));

            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_clear_upgrade_record_run_should_delete_the_journal() {
            let os = MockOs::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let mut file_deleter = MockFileDeleter::new();
            file_deleter
                .expect_delete()
                .withf(|path| path == DouglasFolders::default().upgrade_journal())
                .times(1)
                .returning(|_| Ok(()));
            let mut file_writer = MockFileWriter::new();
            file_writer.expect_write_all().times(0);
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = ClearUpgradeRecord;

            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_clear_upgrade_record_run_should_still_succeed_when_the_journal_cannot_be_removed()
         {
            let os = MockOs::new();
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let mut file_deleter = MockFileDeleter::new();
            file_deleter.expect_delete().returning(|path| {
                Err(FileSystemError::IoErrorAtPath {
                    path: path.to_path_buf(),
                    error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                })
            });
            let mut file_writer = MockFileWriter::new();
            file_writer.expect_write_all().times(0);
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = ClearUpgradeRecord;

            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_restart_previous_version_rollback_should_still_succeed_when_the_journal_cannot_be_removed()
         {
            let mut os = MockOs::new();
            os.expect_execute_with_output().returning(|_, _, _| {
                Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                })
            });
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let mut file_deleter = MockFileDeleter::new();
            file_deleter.expect_delete().returning(|path| {
                Err(FileSystemError::IoErrorAtPath {
                    path: path.to_path_buf(),
                    error: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                })
            });
            let mut file_writer = MockFileWriter::new();
            file_writer.expect_write_all().times(0);
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RestartPreviousVersion::new(Presentation::Plain);

            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_restart_previous_version_rollback_should_keep_the_journal_when_the_previous_version_will_not_start()
         {
            let mut os = MockOs::new();
            os.expect_execute_with_output().returning(|_, _, _| {
                Err(os::OsError::ProccessExitStatusError {
                    name: "douglas".to_string(),
                    code: Some(1),
                    args: Vec::new(),
                    stdout: String::new(),
                    stderr: "no".to_string(),
                })
            });
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let mut file_deleter = MockFileDeleter::new();
            file_deleter.expect_delete().times(0);
            let mut file_writer = MockFileWriter::new();
            file_writer.expect_write_all().times(0);
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RestartPreviousVersion::new(Presentation::Plain);

            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_restart_previous_version_run_should_do_nothing() {
            let mut os = MockOs::new();
            os.expect_execute_with_output().times(0);
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RestartPreviousVersion::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.run(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_restart_previous_version_rollback_should_start_the_installed_version() {
            let mut os = MockOs::new();
            let expected_path = path_to_string(DouglasFolders::default().binary_link());
            os.expect_execute_with_output()
                .withf(move |command, args, _env| {
                    command == expected_path
                        && *args
                            == vec![
                                "--output-style".to_string(),
                                "plain".to_string(),
                                "start".to_string(),
                            ]
                })
                .times(1)
                .returning(|_, _, _| {
                    Ok(std::process::Output {
                        status: std::process::ExitStatus::from_raw(0),
                        stdout: b"".to_vec(),
                        stderr: b"".to_vec(),
                    })
                });
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let mut file_deleter = MockFileDeleter::new();
            file_deleter
                .expect_delete()
                .withf(|path| path == DouglasFolders::default().upgrade_journal())
                .times(1)
                .returning(|_| Ok(()));
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RestartPreviousVersion::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_restart_previous_version_rollback_should_fail_when_the_previous_version_will_not_start()
         {
            let mut os = MockOs::new();
            let expected_path = path_to_string(DouglasFolders::default().binary_link());
            os.expect_execute_with_output()
                .withf(move |command, args, _env| {
                    command == expected_path
                        && *args
                            == vec![
                                "--output-style".to_string(),
                                "plain".to_string(),
                                "start".to_string(),
                            ]
                })
                .times(1)
                .returning(|_, _, _| {
                    Err(os::OsError::ProccessExitStatusError {
                        name: "douglas".to_string(),
                        code: Some(1),
                        args: Vec::new(),
                        stdout: String::new(),
                        stderr: "cannot open the rolodex".to_string(),
                    })
                });
            let heartbeat_reader_factory = heartbeat::MockHeartbeatReaderFactory::new();
            let bract_client = bract_client::MockClient::new();
            let file_renamer = MockFileRenamer::new();
            let links = MockLinks::new();
            let permissions = MockPermissions::new();
            let docker_client = docker::MockClient::new();
            let file_copier = MockFileCopier::new();
            let file_deleter = MockFileDeleter::new();
            let file_writer = MockFileWriter::new();
            let folder = MockFolder::new();
            let mut context = test_context(
                &os,
                FileOperations {
                    renamer: &file_renamer,
                    copier: &file_copier,
                    deleter: &file_deleter,
                    writer: &file_writer,
                    folder: &folder,
                },
                &links,
                &permissions,
                &heartbeat_reader_factory,
                &docker_client,
                &bract_client,
            );

            let mut command = RestartPreviousVersion::new(Presentation::Plain);
            let span = Span::new(CapturingReporter::new(), "test", ScopeKind::Group);

            let result = command.rollback(&span, &mut context).await;

            let Err(err) = result else {
                panic!("should fail");
            };
            assert!(err.to_string().contains("cannot open the rolodex"));
        }
    }
}
