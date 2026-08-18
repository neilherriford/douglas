use crate::{Error, SEEDBANK, SEEDS_ROOT_NAME};
use blueprint::{
    Command,
    bootstrap::{build_boot_reporter, execute_plan, resolve_plan},
    listener::ListenerDefinition,
    service::{
        BootstrapReporting, ServiceDefinition, ServiceState, ServiceUser, discover_service_state,
        plan_service_bootstrap,
    },
};
use config::DouglasFolders;
use credentials::{Credentials, well_known};
use file_system::{Folder, Modes, Permissions};
use log::{ScopeKind, Span};
use std::{path::Path, sync::Arc};

pub static DOUGLAS_SEEDBANK_USER: &str = "douglas-seedbank";
pub static DOUGLAS_SEEDBANK_GROUP: &str = "douglas-seedbank";

type Context<'a> = blueprint::StandardContext<'a>;
type Step<'a> = Box<dyn Command<Context<'a>>>;

pub async fn bootstrap(
    reporting_fd: Option<i32>,
    credentials: &dyn Credentials,
    folder: &dyn Folder,
    permissions: &dyn Permissions,
    douglas_folders: &DouglasFolders,
) -> Result<(), Error> {
    let boot_reporter =
        build_boot_reporter(douglas_folders.service_log_file(SEEDBANK), reporting_fd);

    let guard = Span::new(
        Arc::clone(&boot_reporter),
        "Bootstrapping douglas-seedbank system",
        log::ScopeKind::Group,
    )
    .start_guard();

    let root_path = douglas_folders.service_root(SEEDBANK);
    let definition = service_definition(douglas_folders);

    let state = {
        let mut state_observer = StateObserver::new(credentials, folder);
        state_observer
            .discover(
                guard.span(),
                &definition,
                &root_path,
                credentials,
                folder,
                permissions,
            )
            .await?
    };

    let plan = match resolve_plan(guard.span(), create_plan(&definition, state)) {
        Ok(plan) => plan,
        Err(err) => return guard.finish(Err(err)),
    };

    let mut context = Context::new(credentials, folder, permissions);
    let result = execute_plan(guard.span(), plan, &mut context, |reason| {
        Error::FailedBoostrap(vec![reason])
    })
    .await;

    guard.finish(result)
}

pub fn service_definition(douglas_folders: &DouglasFolders) -> ServiceDefinition {
    let mut seeds = douglas_folders.service_root(SEEDBANK);
    seeds.push(SEEDS_ROOT_NAME);

    ServiceDefinition::with_sockets(
        ServiceUser::create_managed(DOUGLAS_SEEDBANK_USER),
        DOUGLAS_SEEDBANK_GROUP,
        vec![
            (seeds, Modes::OwnerReadWriteExecuteGroupReadWriteExecute),
            (
                douglas_folders.socket_dir(SEEDBANK),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
            ),
            (
                douglas_folders.socket_dir(seedling_registration_types::SOCKET_NAME),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
            ),
            (
                douglas_folders.log_dir(SEEDBANK),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
        ],
        vec![
            ListenerDefinition::new(
                &douglas_folders.socket_file(SEEDBANK),
                DOUGLAS_SEEDBANK_USER,
                well_known::DOUGLAS_RESIN_SEEDBANK_GROUP,
                Modes::OwnerReadWriteGroupReadWrite,
            ),
            ListenerDefinition::new(
                &douglas_folders.socket_file(seedling_registration_types::SOCKET_NAME),
                DOUGLAS_SEEDBANK_USER,
                well_known::DOUGLAS_RESIN_SEEDBANK_GROUP,
                Modes::OwnerReadWriteGroupReadWrite,
            ),
        ],
        &[well_known::DOUGLAS_RESIN_SEEDBANK_GROUP],
        BootstrapReporting::Pipe,
    )
}

#[cfg(test)]
mod tests {
    use super::service_definition;
    use config::DouglasFolders;
    use credentials::well_known::DOUGLAS_RESIN_SEEDBANK_GROUP;

    #[test]
    fn test_service_definition_should_declare_both_the_main_and_registration_sockets() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        assert_eq!(
            definition
                .owned_sockets
                .iter()
                .map(|listener| listener.socket_path.clone())
                .collect::<Vec<_>>(),
            vec![
                douglas_folders.socket_file(crate::SEEDBANK),
                douglas_folders.socket_file(seedling_registration_types::SOCKET_NAME),
            ]
        );
    }

    #[test]
    fn test_service_definition_should_own_the_main_socket_with_the_resin_seedbank_group() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        let main_socket = definition
            .owned_sockets
            .iter()
            .find(|listener| listener.socket_path == douglas_folders.socket_file(crate::SEEDBANK))
            .expect("main socket should be declared");

        assert_eq!(main_socket.owning_group, DOUGLAS_RESIN_SEEDBANK_GROUP);
    }

    #[test]
    fn test_service_definition_should_own_the_registration_socket_with_the_resin_seedbank_group() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        let registration_socket = definition
            .owned_sockets
            .iter()
            .find(|listener| {
                listener.socket_path
                    == douglas_folders.socket_file(seedling_registration_types::SOCKET_NAME)
            })
            .expect("registration socket should be declared");

        assert_eq!(
            registration_socket.owning_group,
            DOUGLAS_RESIN_SEEDBANK_GROUP
        );
    }

    #[test]
    fn test_service_definition_should_own_the_registration_socket_directory() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        assert!(definition.owned_folders.iter().any(|(path, _mode)| path
            == &douglas_folders.socket_dir(seedling_registration_types::SOCKET_NAME)));
    }

    #[test]
    fn test_service_definition_should_declare_the_resin_seedbank_group_as_an_additional_group() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        assert_eq!(
            definition.additional_groups,
            vec![DOUGLAS_RESIN_SEEDBANK_GROUP.to_string()]
        );
    }
}

fn create_plan<'a>(definition: &ServiceDefinition, state: State) -> Result<Vec<Step<'a>>, Error> {
    if state.is_root {
        return Err(Error::CannotBeRoot);
    }

    if !state.root_path_exists {
        return Err(Error::MissingRootPath);
    }

    Ok(plan_service_bootstrap(definition, &state.service))
}

#[derive(Default)]
struct State {
    is_root: bool,
    root_path_exists: bool,
    service: ServiceState,
}

struct StateObserver<'a> {
    credentials: &'a dyn Credentials,
    folder: &'a dyn Folder,
}

impl<'a> StateObserver<'a> {
    pub fn new(credentials: &'a dyn Credentials, folder: &'a dyn Folder) -> Self {
        Self {
            credentials,
            folder,
        }
    }

    pub async fn discover(
        &mut self,
        span: &Span,
        definition: &ServiceDefinition,
        root_path: &Path,
        credentials: &dyn Credentials,
        folder: &dyn Folder,
        permissions: &dyn Permissions,
    ) -> Result<State, Error> {
        let guard = span
            .create_child(
                "Starting seedbank system, discovering current state",
                ScopeKind::Phase,
            )
            .start_guard();

        let root_path_exists = self.folder.exists(root_path);

        let mut result = State {
            is_root: self.credentials.is_root(),
            root_path_exists,
            ..Default::default()
        };

        if result.is_root || !result.root_path_exists {
            return guard.finish(Ok(result));
        }

        result.service = discover_service_state(definition, credentials, folder, permissions)?;

        guard.finish(Ok(result))
    }
}
