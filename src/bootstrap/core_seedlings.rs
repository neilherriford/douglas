use blueprint::{
    Command, GroupMembershipRequirement, HasCredentials, HasFolder, HasPermissions, RunningStatus,
    bootstrap::{execute_plan, resolve_plan},
    commands::{AddUserToGroup, CreateGroup},
    listener::{LivenessCheck, check_liveness},
    service::{
        BootstrapReporting, ServiceDefinition, ServiceState, discover_service_state,
        plan_service_bootstrap,
    },
};
use command_fds::{CommandFdExt, FdMapping, FdMappingCollision};
use config::DouglasFolders;
use credentials::{Credentials, well_known::DOUGLAS_ADMIN_GROUP};
use docker::VersionedImageName;
use file_system::RelativePathError;
use file_system::{FileSystemError, Folder, Permissions};
use log::{Level, Outcome, Reporter, ScopeKind, Span};
use os::{EnvironmentVariableReader, Os};
use os_pipe::{PipeReader, PipeWriter};
use seedbank::{Mount, MountContents, MountType, Name, NameParseError, SeedlingDefinition};
use std::{
    collections::HashMap,
    env::VarError,
    os::{fd::OwnedFd, unix::io::AsRawFd},
    sync::Arc,
    time::{Duration, Instant},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};
use thiserror::Error;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BootstrapError {
    #[error("Must be root to proceed")]
    MustBeRoot,
    #[error("Pipe required")]
    PipeRequired,
    #[error("Spawn error: {0}")]
    SpawnError(#[from] FdMappingCollision),
    #[error("Timed out waiting for {0} to start (5 minutes exceeded)")]
    StartTimeout(String),
    #[error("Service '{0}' has no configured control socket")]
    MissingControlSocket(&'static str),
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),
}

#[derive(Debug, Error)]
pub enum SeedDefinitionError {
    #[error("Relative path error")]
    RelativePathError(#[from] RelativePathError),
    #[error("Name parse error")]
    NameParseError(#[from] NameParseError),
}

pub fn traefik() -> Result<SeedlingDefinition, SeedDefinitionError> {
    let mount_name: Name = "config".parse()?;

    Ok(SeedlingDefinition::new(
        VersionedImageName::specific("traefik", "v3.7.7"),
        HashMap::from([(
            mount_name,
            Mount::build(
                MountType::Persisted,
                PathBuf::from("/etc/traefik"),
                HashSet::from([
                    MountContents::file("traefik.yml", generate_default_static_definition())?,
                    MountContents::folder_only("dynamic")?,
                ]),
            ),
        )]),
    ))
}

fn generate_default_static_definition() -> &'static [u8] {
    r#"entryPoints:
  web:
    address: ":80"

providers:
  file:
    directory: "/etc/traefik/dynamic"
    watch: true

log:
  level: INFO
"#
    .as_bytes()
}

type Step<'a> = Box<dyn Command<Context<'a>>>;

fn push_step<'a>(steps: &mut Vec<Step<'a>>, command: impl Command<Context<'a>> + 'static) {
    steps.push(Box::new(command));
}

struct Context<'a> {
    bract_client: &'a dyn bract::Client,
}

#[derive(Default)]
struct State {
    core_seedlings_missing: Vec<Name>,
    core_seedlings_needing_start: Vec<Name>,
}

impl<'a> StateObserver<'a> {
    pub fn new(
        credentials: &'a dyn Credentials,
        environment_variable_reader: &'a dyn EnvironmentVariableReader,
        folder: &'a dyn Folder,
        permissions: &'a dyn Permissions,
    ) -> Self {
        Self {
            credentials,
            environment_variable_reader,
            folder,
            permissions,
        }
    }

    pub fn discover(
        &mut self,
        span: &Span,
        douglas_folders: &DouglasFolders,
    ) -> Result<State, BootstrapError> {
        todo!()
        //     let guard = span
        //         .create_child(
        //             "Starting douglas system, discovering current state",
        //             ScopeKind::Phase,
        //         )
        //         .start_guard();

        //     if !self.credentials.is_root() {
        //         return guard.finish(Ok(State::default()));
        //     }

        //     let mut result = State {
        //         is_root: true,
        //         ..Default::default()
        //     };

        //     self.check_admin_group_membership(guard.span(), &mut result);

        //     let services = match known_services(douglas_folders) {
        //         Ok(services) => services,
        //         Err(err) => return guard.finish(Err(err)),
        //     };

        //     for service in services {
        //         let status = check_liveness(guard.span(), &service.liveness);
        //         if status != RunningStatus::Running {
        //             let service_state = match discover_service_state(
        //                 &service.definition,
        //                 self.credentials,
        //                 self.folder,
        //                 self.permissions,
        //             ) {
        //                 Ok(service_state) => service_state,
        //                 Err(err) => return guard.finish(Err(err.into())),
        //             };
        //             result.services_needing_start.push((service, service_state));
        //         }
        //     }

        //     guard.finish(Ok(result))
        // }

        // fn check_admin_group_membership(&mut self, span: &Span, result: &mut State) {
        //     let (non_sudoer, valid_non_sudoer) = self.get_non_sudoer(span);
        //     if self.credentials.group_exists(DOUGLAS_ADMIN_GROUP) {
        //         if valid_non_sudoer
        //             && !self
        //                 .credentials
        //                 .group_memberships(DOUGLAS_ADMIN_GROUP)
        //                 .contains(&non_sudoer)
        //         {
        //             result
        //                 .group_members_missing
        //                 .push(GroupMembershipRequirement::new(
        //                     DOUGLAS_ADMIN_GROUP,
        //                     &non_sudoer,
        //                 ));
        //         }
        //     } else {
        //         result.groups_missing.push(DOUGLAS_ADMIN_GROUP.to_string());
        //         if valid_non_sudoer {
        //             result
        //                 .group_members_missing
        //                 .push(GroupMembershipRequirement::new(
        //                     DOUGLAS_ADMIN_GROUP,
        //                     &non_sudoer,
        //                 ));
        //         }
        //     }
        // }

        // fn get_non_sudoer(&self, span: &Span) -> (String, bool) {
        //     match self.environment_variable_reader.read("SUDO_USER") {
        //         Ok(user_name) => {
        //             let valid = user_name != credentials::ROOT_USER_NAME;
        //             (user_name, valid)
        //         }
        //         Err(VarError::NotPresent) => (credentials::ROOT_USER_NAME.to_string(), false),
        //         Err(VarError::NotUnicode(_)) => {
        //             span.message(Level::Warn, &format!(
        //                         "Could not determine initiating user?  You will need to manually add the \
        //                             account you wish to interact with the Douglas CLI to the '{DOUGLAS_ADMIN_GROUP}' \
        //                             manually!"
        //                     ));
        //             (credentials::ROOT_USER_NAME.to_string(), false)
        //         }
        //     }
    }
}

fn create_plan<'a>(state: State) -> Result<Vec<Step<'a>>, BootstrapError> {
    // if !state.is_root {
    //     return Err(BootstrapError::MustBeRoot);
    // }

    // let mut result = Vec::new();

    // for group_name in &state.groups_missing {
    //     push_step(&mut result, CreateGroup::new(group_name));
    // }

    // for membership in &state.group_members_missing {
    //     push_step(
    //         &mut result,
    //         AddUserToGroup::new(&membership.user_name, &membership.group_name),
    //     );
    // }

    // for (service, service_state) in &state.services_needing_start {
    //     for step in plan_service_bootstrap::<Context<'a>>(&service.definition, service_state) {
    //         result.push(step);
    //     }
    // }

    // for (service, _) in &state.services_needing_start {
    //     if matches!(service.bootstrap_reporting, BootstrapReporting::Pipe) {
    //         push_step(&mut result, CreatePipe::new(service.name));
    //     }
    // }

    // for (service, _) in &state.services_needing_start {
    //     let needs_reporting_pipe = matches!(service.bootstrap_reporting, BootstrapReporting::Pipe);
    //     push_step(
    //         &mut result,
    //         StartService::new(service.name, needs_reporting_pipe),
    //     );
    // }

    // for (service, _) in state.services_needing_start {
    //     push_step(
    //         &mut result,
    //         WaitForServiceReady::new(service.name, service.liveness),
    //     );
    // }

    // Ok(result)
    todo!();
}

pub async fn perform(
    reporter: Arc<dyn Reporter>,
    // plan_only: bool,
    // credentials: Arc<dyn Credentials>,
    // permissions: Arc<dyn Permissions>,
    // environment_variable_reader: Arc<dyn EnvironmentVariableReader>,
    // folder: Arc<dyn Folder>,
    // os: Arc<dyn Os>,
    // douglas_folders: DouglasFolders,
) {
}
