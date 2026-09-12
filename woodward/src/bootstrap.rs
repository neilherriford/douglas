use crate::WOODWARD;
use blueprint::{
    listener::LivenessCheck,
    service::{ServiceDefinition, ServiceUser},
};
use config::DouglasFolders;
use credentials::well_known::DOUGLAS_ADMIN_GROUP;
use file_system::Modes;
use std::time::Duration;

pub static DOUGLAS_WOODWARD_USER: &str = "woodward";
pub static DOUGLAS_WOODWARD_GROUP: &str = "woodward";

const HEARTBEAT_MAX_AGE_SECONDS: u64 = 15;

pub fn service_definition(douglas_folders: &DouglasFolders) -> ServiceDefinition {
    ServiceDefinition::new(
        ServiceUser::create_managed(DOUGLAS_WOODWARD_USER),
        DOUGLAS_WOODWARD_GROUP,
        vec![
            (
                douglas_folders.log_dir(WOODWARD),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecute,
            ),
            (
                douglas_folders.supervisor_dir(),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
            ),
            (
                douglas_folders.heartbeat_dir(WOODWARD),
                Modes::OwnerReadWriteExecuteGroupReadWriteExecuteOtherExecute,
            ),
        ],
        &[DOUGLAS_ADMIN_GROUP],
        blueprint::service::BootstrapReporting::None,
        Some(LivenessCheck::Heartbeat {
            path: douglas_folders.service_heartbeat_file(WOODWARD),
            max_age: Duration::from_secs(HEARTBEAT_MAX_AGE_SECONDS),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_definition_should_declare_the_admin_group_as_an_additional_group() {
        let douglas_folders = DouglasFolders::new();

        let definition = service_definition(&douglas_folders);

        assert_eq!(
            definition.additional_groups,
            vec![DOUGLAS_ADMIN_GROUP.to_string()]
        );
    }
}
