use crate::{
    Command, FolderModeRequirement, FolderOwnershipRequirement, HasCredentials, HasFolder,
    HasPermissions, commands, listener::ListenerDefinition,
};
use credentials::Credentials;
use file_system::{FileSystemError, Folder, Modes, Permissions};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum ServiceUser {
    Managed(String),
    System(String),
}

impl ServiceUser {
    pub fn create_managed(name: &str) -> Self {
        Self::Managed(name.to_string())
    }
    pub fn create_system(name: &str) -> Self {
        Self::System(name.to_string())
    }

    pub fn name(&self) -> String {
        match self {
            Self::Managed(name) => name.clone(),
            Self::System(name) => name.clone(),
        }
    }
}

#[derive(Debug)]
pub enum BootstrapReporting {
    None,
    Pipe,
}

#[derive(Debug)]
pub struct ServiceDefinition {
    pub user: ServiceUser,
    pub group: String,
    pub owned_folders: Vec<(PathBuf, Modes)>,
    pub owned_sockets: Vec<ListenerDefinition>,
    pub bootstrap_reporting: BootstrapReporting,
}

impl ServiceDefinition {
    pub fn with_sockets(
        user: ServiceUser,
        group: &str,
        owned_folders: Vec<(PathBuf, Modes)>,
        owned_sockets: Vec<ListenerDefinition>,
        bootstrap_reporting: BootstrapReporting,
    ) -> Self {
        Self {
            user,
            group: group.to_string(),
            owned_folders,
            owned_sockets,
            bootstrap_reporting,
        }
    }

    pub fn new(
        user: ServiceUser,
        group: &str,
        owned_folders: Vec<(PathBuf, Modes)>,
        bootstrap_reporting: BootstrapReporting,
    ) -> Self {
        Self::with_sockets(user, group, owned_folders, Vec::new(), bootstrap_reporting)
    }
}

#[derive(Default)]
pub struct ServiceState {
    pub group_missing: bool,
    pub user_missing: bool,
    pub group_membership_missing: bool,
    pub folders_missing: Vec<PathBuf>,
    pub folders_missing_ownership: Vec<FolderOwnershipRequirement>,
    pub folders_missing_mode: Vec<FolderModeRequirement>,
}

pub fn discover_service_state(
    definition: &ServiceDefinition,
    credentials: &dyn Credentials,
    folder: &dyn Folder,
    permissions: &dyn Permissions,
) -> Result<ServiceState, FileSystemError> {
    let mut state = ServiceState {
        group_missing: !credentials.group_exists(&definition.group),
        user_missing: matches!(
            &definition.user, ServiceUser::Managed(name)
            if !credentials.user_exists(name)
        ),
        ..Default::default()
    };

    state.group_membership_missing = matches!(
        &definition.user,
        ServiceUser::Managed(name)
        if !state.group_missing
            && !credentials
                .group_memberships(&definition.group)
                .iter()
                .any(|member| member == name)
    );

    for (path, expected_mode) in &definition.owned_folders {
        discover_folder_state(
            &mut state,
            definition,
            path,
            *expected_mode,
            folder,
            permissions,
        )?;
    }

    Ok(state)
}

fn discover_folder_state(
    state: &mut ServiceState,
    definition: &ServiceDefinition,
    path: &Path,
    expected_mode: Modes,
    folder: &dyn Folder,
    permissions: &dyn Permissions,
) -> Result<(), FileSystemError> {
    if !folder.exists(path) {
        state.folders_missing.push(path.to_path_buf());
        state
            .folders_missing_ownership
            .push(FolderOwnershipRequirement::new(
                path.to_path_buf(),
                &definition.user.name(),
                &definition.group,
            ));
        state.folders_missing_mode.push(FolderModeRequirement {
            path: path.to_path_buf(),
            mode: expected_mode,
        });
        return Ok(());
    }

    let (owning_user, owning_group) = permissions.get_user_and_group_ownership(path)?;
    if owning_user != definition.user.name() || owning_group != definition.group {
        state
            .folders_missing_ownership
            .push(FolderOwnershipRequirement::new(
                path.to_path_buf(),
                &definition.user.name(),
                &definition.group,
            ));
    }

    let actual_mode = permissions.get_mode(path)?;
    if actual_mode != expected_mode {
        state.folders_missing_mode.push(FolderModeRequirement {
            path: path.to_path_buf(),
            mode: expected_mode,
        });
    }

    Ok(())
}

pub fn plan_service_bootstrap<C>(
    definition: &ServiceDefinition,
    state: &ServiceState,
) -> Vec<Box<dyn Command<C>>>
where
    C: HasCredentials + HasFolder + HasPermissions,
{
    let mut steps: Vec<Box<dyn Command<C>>> = Vec::new();

    if state.group_missing {
        steps.push(Box::new(commands::CreateGroup::new(&definition.group)));
    }

    if state.user_missing {
        steps.push(Box::new(commands::CreateUser::new(
            &definition.user.name(),
            &definition.group,
        )));
    } else if state.group_membership_missing {
        steps.push(Box::new(commands::AddUserToGroup::new(
            &definition.user.name(),
            &definition.group,
        )));
    }

    for folder in &state.folders_missing {
        steps.push(Box::new(commands::CreateFolder::new(folder.clone())));
    }

    for requirement in &state.folders_missing_ownership {
        steps.push(Box::new(commands::SetOwnership::new(
            requirement.path.clone(),
            &requirement.owning_user_name,
            &requirement.owning_group_name,
        )));
    }

    for requirement in &state.folders_missing_mode {
        steps.push(Box::new(commands::SetMode::new(
            requirement.path.clone(),
            requirement.mode,
        )));
    }

    steps
}

#[cfg(test)]
mod tests {
    mod discover_service_state {
        use crate::service::{
            BootstrapReporting, ServiceDefinition, ServiceUser, discover_service_state,
        };
        use credentials::MockCredentials;
        use file_system::{MockFolder, MockPermissions, Modes};
        use std::path::PathBuf;

        fn definition() -> ServiceDefinition {
            ServiceDefinition::new(
                ServiceUser::create_managed("foo"),
                "foo",
                vec![(PathBuf::from("/var/lib/foo"), Modes::OwnerReadWrite)],
                BootstrapReporting::Pipe,
            )
        }

        fn unmanaged_user_definition() -> ServiceDefinition {
            ServiceDefinition::new(
                ServiceUser::create_system(credentials::ROOT_USER_NAME),
                "douglas-admin",
                vec![(PathBuf::from("/var/lib/foo"), Modes::OwnerReadWrite)],
                BootstrapReporting::None,
            )
        }

        #[test]
        fn test_should_flag_missing_group() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();

            credentials
                .given_group_does_not_exist("foo")
                .given_user_exists("foo")
                .given_user_memberships("foo", vec!["foo"]);

            folder.given_exists("/var/lib/foo");
            permissions.given_ownership_and_mode(
                "/var/lib/foo",
                "foo",
                "foo",
                Modes::OwnerReadWrite,
            );

            let state = discover_service_state(&definition(), &credentials, &folder, &permissions)
                .expect("should discover");

            assert!(state.group_missing);
        }

        #[test]
        fn test_should_flag_missing_user() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();

            credentials
                .given_group_exists("foo")
                .given_user_does_not_exist("foo")
                .given_user_memberships("foo", vec![]);

            folder.given_exists("/var/lib/foo");
            permissions.given_ownership_and_mode(
                "/var/lib/foo",
                "foo",
                "foo",
                Modes::OwnerReadWrite,
            );

            let state = discover_service_state(&definition(), &credentials, &folder, &permissions)
                .expect("should discover");

            assert!(state.user_missing);
        }

        #[test]
        fn test_should_flag_missing_group_membership_when_group_and_user_exist() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();

            credentials
                .given_group_exists("foo")
                .given_user_exists("foo")
                .given_user_memberships("foo", vec!["bar"]);

            folder.given_exists("/var/lib/foo");
            permissions.given_ownership_and_mode(
                "/var/lib/foo",
                "foo",
                "foo",
                Modes::OwnerReadWrite,
            );

            let state = discover_service_state(&definition(), &credentials, &folder, &permissions)
                .expect("should discover");

            assert!(state.group_membership_missing);
        }

        #[test]
        fn test_should_not_flag_group_membership_when_already_a_member() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();

            credentials
                .given_group_exists("foo")
                .given_user_exists("foo")
                .given_user_memberships("foo", vec!["foo"]);
            folder.given_exists("/var/lib/foo");
            permissions.given_ownership_and_mode(
                "/var/lib/foo",
                "foo",
                "foo",
                Modes::OwnerReadWrite,
            );

            let state = discover_service_state(&definition(), &credentials, &folder, &permissions)
                .expect("should discover");

            assert!(!state.group_membership_missing);
        }

        #[test]
        fn test_should_flag_missing_folder_for_creation_ownership_and_mode() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let permissions = MockPermissions::new();

            credentials
                .given_group_exists("foo")
                .given_user_exists("foo")
                .given_user_memberships("foo", vec!["foo"]);

            folder.given_does_not_exist("/var/lib/foo");

            let state = discover_service_state(&definition(), &credentials, &folder, &permissions)
                .expect("should discover");

            assert_eq!(state.folders_missing, vec![PathBuf::from("/var/lib/foo")]);
            assert_eq!(state.folders_missing_ownership.len(), 1);
            assert_eq!(state.folders_missing_mode.len(), 1);
        }

        #[test]
        fn test_should_flag_wrong_ownership_on_existing_folder() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();

            credentials
                .given_group_exists("foo")
                .given_user_exists("foo")
                .given_user_memberships("foo", vec!["foo"]);

            folder.given_exists("/var/lib/foo");
            permissions.given_ownership_and_mode(
                "/var/lib/foo",
                "someone_else",
                "foo",
                Modes::OwnerReadWrite,
            );

            let state = discover_service_state(&definition(), &credentials, &folder, &permissions)
                .expect("should discover");

            assert_eq!(state.folders_missing_ownership.len(), 1);
            assert!(state.folders_missing_mode.is_empty());
        }

        #[test]
        fn test_should_flag_wrong_mode_on_existing_folder() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();

            credentials
                .given_group_exists("foo")
                .given_user_exists("foo")
                .given_user_memberships("foo", vec!["foo"]);

            folder.given_exists("/var/lib/foo");
            permissions.given_ownership_and_mode(
                "/var/lib/foo",
                "foo",
                "foo",
                Modes::OwnerReadWriteGroupRead,
            );

            let state = discover_service_state(&definition(), &credentials, &folder, &permissions)
                .expect("should discover");

            assert!(state.folders_missing_ownership.is_empty());
            assert_eq!(state.folders_missing_mode.len(), 1);
        }

        #[test]
        fn test_should_flag_nothing_when_everything_already_correct() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();

            credentials
                .given_group_exists("foo")
                .given_user_exists("foo")
                .given_user_memberships("foo", vec!["foo"]);

            folder.given_exists("/var/lib/foo");
            permissions.given_ownership_and_mode(
                "/var/lib/foo",
                "foo",
                "foo",
                Modes::OwnerReadWrite,
            );

            let state = discover_service_state(&definition(), &credentials, &folder, &permissions)
                .expect("should discover");

            assert!(!state.group_missing);
            assert!(!state.user_missing);
            assert!(!state.group_membership_missing);
            assert!(state.folders_missing.is_empty());
            assert!(state.folders_missing_ownership.is_empty());
            assert!(state.folders_missing_mode.is_empty());
        }

        #[test]
        fn test_should_never_flag_user_or_membership_when_user_is_unmanaged() {
            let mut credentials = MockCredentials::new();
            let mut folder = MockFolder::new();
            let mut permissions = MockPermissions::new();

            credentials
                .given_group_does_not_exist("douglas-admin")
                .given_user_memberships("douglas-admin", vec![]);

            folder.given_exists("/var/lib/foo");
            permissions.given_ownership_and_mode(
                "/var/lib/foo",
                "root",
                "douglas-admin",
                Modes::OwnerReadWrite,
            );

            let state = discover_service_state(
                &unmanaged_user_definition(),
                &credentials,
                &folder,
                &permissions,
            )
            .expect("should discover");

            assert!(state.group_missing);
            assert!(!state.user_missing);
            assert!(!state.group_membership_missing);
        }
    }

    mod plan_service_bootstrap {
        use crate::{
            Command, FolderModeRequirement, FolderOwnershipRequirement, HasCredentials, HasFolder,
            HasPermissions,
            service::{
                BootstrapReporting, ServiceDefinition, ServiceState, ServiceUser,
                plan_service_bootstrap,
            },
        };
        use credentials::Credentials;
        use file_system::{Folder, Permissions};
        use std::path::PathBuf;

        struct TestContext;

        impl HasCredentials for TestContext {
            fn credentials(&self) -> &dyn Credentials {
                unimplemented!("not exercised by these tests")
            }
        }

        impl HasFolder for TestContext {
            fn folder(&self) -> &dyn Folder {
                unimplemented!("not exercised by these tests")
            }
        }

        impl HasPermissions for TestContext {
            fn permissions(&self) -> &dyn Permissions {
                unimplemented!("not exercised by these tests")
            }
        }

        fn definition() -> ServiceDefinition {
            ServiceDefinition::new(
                ServiceUser::create_managed("foo"),
                "foo",
                Vec::new(),
                BootstrapReporting::Pipe,
            )
        }

        fn descriptions(steps: &[Box<dyn Command<TestContext>>]) -> Vec<String> {
            steps.iter().map(std::string::ToString::to_string).collect()
        }

        #[test]
        fn test_should_produce_no_steps_when_nothing_is_missing() {
            let state = ServiceState::default();

            let steps = plan_service_bootstrap::<TestContext>(&definition(), &state);

            assert!(steps.is_empty());
        }

        #[test]
        fn test_should_create_group_and_user_when_both_missing() {
            let state = ServiceState {
                group_missing: true,
                user_missing: true,
                ..Default::default()
            };

            let steps = plan_service_bootstrap::<TestContext>(&definition(), &state);
            let descriptions = descriptions(&steps);

            assert_eq!(descriptions.len(), 2);
            assert!(descriptions[0].contains("Create group 'foo'"));
            assert!(descriptions[1].contains("Create user 'foo'"));
        }

        #[test]
        fn test_should_not_create_user_when_only_group_missing() {
            let state = ServiceState {
                group_missing: true,
                ..Default::default()
            };

            let steps = plan_service_bootstrap::<TestContext>(&definition(), &state);
            let descriptions = descriptions(&steps);

            assert_eq!(descriptions.len(), 1);
            assert!(descriptions[0].contains("Create group"));
        }

        #[test]
        fn test_should_add_user_to_group_when_user_exists_but_not_a_member() {
            let state = ServiceState {
                group_membership_missing: true,
                ..Default::default()
            };

            let steps = plan_service_bootstrap::<TestContext>(&definition(), &state);
            let descriptions = descriptions(&steps);

            assert_eq!(descriptions.len(), 1);
            assert!(descriptions[0].contains("Add user 'foo' to group 'foo'"));
        }

        #[test]
        fn test_should_not_add_user_to_group_when_user_was_just_created() {
            let state = ServiceState {
                user_missing: true,
                group_membership_missing: true,
                ..Default::default()
            };

            let steps = plan_service_bootstrap::<TestContext>(&definition(), &state);
            let descriptions = descriptions(&steps);

            assert_eq!(descriptions.len(), 1);
            assert!(descriptions[0].contains("Create user"));
        }

        #[test]
        fn test_should_create_folder_set_ownership_and_mode_when_folder_missing() {
            let state = ServiceState {
                folders_missing: vec![PathBuf::from("/var/lib/foo")],
                folders_missing_ownership: vec![FolderOwnershipRequirement::new(
                    PathBuf::from("/var/lib/foo"),
                    "foo",
                    "foo",
                )],
                folders_missing_mode: vec![FolderModeRequirement {
                    path: PathBuf::from("/var/lib/foo"),
                    mode: file_system::Modes::OwnerReadWrite,
                }],
                ..Default::default()
            };

            let steps = plan_service_bootstrap::<TestContext>(&definition(), &state);
            let descriptions = descriptions(&steps);

            assert_eq!(descriptions.len(), 3);
            assert!(descriptions[0].contains("Create folder"));
            assert!(descriptions[1].contains("Set ownership"));
            assert!(descriptions[2].contains("Set mode"));
        }
    }
}
