use blueprint::{Command, CommandExecutor, JournalingExecutor};
use config::constants::DOUGLAS_APP_GROUP;
use credentials::Credentials;
use postcard::{from_bytes, to_allocvec};
use serde::{Deserialize, Serialize};
use sled::{Db, Tree};
use std::{
    collections::{HashMap, HashSet},
    hash::Hasher,
    sync::Arc,
};
use thiserror::Error;

#[derive(Serialize, Deserialize, Debug)]
pub struct ServiceCredentials {
    pub user: Credential,
    pub group: Credential,
}

#[derive(Error, Debug)]
pub enum ServiceAccountManagerError {
    #[error("Missing definition error: {0}")]
    MissingDefinition(String),

    #[error("IO error: {0}")]
    IoError(#[from] sled::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] postcard::Error),

    #[error("UTF Serialization error: {0}")]
    UtfSerializationError(#[from] std::str::Utf8Error),
    #[error("Credentials error: {0}")]
    CredentialsError(#[from] credentials::CredentialsError),
    #[error("Invalid service name: '{given}', {reason}")]
    InvalidServiceName { given: String, reason: String },
    #[error("Name not found: {name}.  {details}")]
    NotFound { name: String, details: String },
    #[error("Configuration mismatch: {0}")]
    ConfigurationMismatch(String),
}

type Step = Box<dyn Command<Context, Error = ServiceAccountManagerError>>;

fn push_step(
    steps: &mut Vec<Step>,
    command: impl Command<Context, Error = ServiceAccountManagerError> + 'static,
) {
    steps.push(Box::new(command));
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Credential {
    pub display_name: String,
    pub system_name: String,
    pub id: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct ShareGroupDefinition {
    group: Credential,
    members: HashSet<Credential>,
}

impl std::hash::Hash for ShareGroupDefinition {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.group.hash(state);
        let mut members: Vec<_> = self.members.iter().collect();
        members.sort_unstable_by_key(|c| &c.system_name);
        members.hash(state);
    }
}

pub trait ServiceAccountManager {
    fn find_or_create(
        &mut self,
        service_name: &str,
    ) -> Result<ServiceDefinition, ServiceAccountManagerError>;
}

pub struct LocalServiceAccountManager {
    logger: Arc<dyn log::Logger>,
    credentials: Arc<dyn Credentials>,
    group_system_name_to_service_name: Tree,
    service_name_to_definition: Tree,
    user_system_name_to_service_name: Tree,

    group_system_name_to_display_name: Tree,
    user_system_name_to_display_name: Tree,

    plan_factory: PlanFactory,
    command_executor: JournalingExecutor<Context, ServiceAccountManagerError>,
}

impl LocalServiceAccountManager {
    pub fn build(
        bract_data: &Db,
        logger: Arc<dyn log::Logger>,
        credentials: Arc<dyn Credentials>,
    ) -> Result<Self, ServiceAccountManagerError> {
        let group_system_name_to_service_name =
            bract_data.open_tree("group_name_to_service_name")?;
        let service_name_to_definition = bract_data.open_tree("service_name_to_definition")?;
        let user_system_name_to_service_name = bract_data.open_tree("user_name_to_service_name")?;

        let user_system_name_to_display_name = bract_data.open_tree("user_name_to_display_name")?;
        let group_system_name_to_display_name =
            bract_data.open_tree("group_name_to_display_name")?;

        let plan_factory = PlanFactory::new(
            Arc::clone(&credentials),
            service_name_to_definition.clone(),
            group_system_name_to_service_name.clone(),
            group_system_name_to_display_name.clone(),
            user_system_name_to_service_name.clone(),
            user_system_name_to_display_name.clone(),
        );

        let command_executor = JournalingExecutor::new(Arc::clone(&logger));

        Ok(Self {
            logger: Arc::clone(&logger),
            credentials: Arc::clone(&credentials),
            service_name_to_definition,
            group_system_name_to_service_name,
            user_system_name_to_service_name,
            user_system_name_to_display_name,
            group_system_name_to_display_name,
            plan_factory,
            command_executor,
        })
    }
}

impl ServiceAccountManager for LocalServiceAccountManager {
    fn find_or_create(
        &mut self,
        service_name: &str,
    ) -> Result<ServiceDefinition, ServiceAccountManagerError> {
        self.logger.info("Creating proposal…");
        let proposal = self.plan_factory.create(service_name)?;

        self.logger.info("Executing…");

        let mut context = Context {
            credentials: Arc::clone(&self.credentials),
            service_name_to_definition: self.service_name_to_definition.clone(),
            group_system_name_to_service_name: self.group_system_name_to_service_name.clone(),
            user_system_name_to_service_name: self.user_system_name_to_service_name.clone(),
            group_system_name_to_display_name: self.group_system_name_to_display_name.clone(),
            user_system_name_to_display_name: self.user_system_name_to_display_name.clone(),
            requested_service_name: service_name.to_string(),
        };

        let what = self.command_executor.run(&mut context, proposal);

        todo!()
    }
}

mod hash {
    // See: https://en.wikipedia.org/wiki/Fowler–Noll–Vo_hash_function
    pub fn fnv1a(input: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in input.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }
}

mod names {
    use crate::server::service_account_manager::{
        ParsedValueAccessor, ServiceAccountManagerError, hash, parse_string,
    };
    use sled::Tree;

    static MAX_SERVICE_NAME_LENGTH: usize = 27;
    static TRUNCATED_SERVICE_NAME_LENGTH: usize = 10;
    static NAME_PREFIX: &str = "doug-";

    pub fn create_account_display_name(service_name: &str) -> String {
        format!("{NAME_PREFIX}{service_name}")
    }

    pub fn create_share_group_display_name(service_name: &str, share_group_name: &str) -> String {
        format!("{NAME_PREFIX}{service_name}_{share_group_name}")
    }

    fn clean_name(input: &str) -> String {
        input
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' => c,
                ' ' | '-' | '_' => '_',
                _ => '_',
            })
            .collect()
    }

    pub fn create_system_name(
        display_name: &str,
        system_name_to_display_name: &Tree,
    ) -> Result<String, ServiceAccountManagerError> {
        let clean_name = clean_name(display_name);

        if clean_name.len() > MAX_SERVICE_NAME_LENGTH {
            let truncated = match clean_name.get(0..TRUNCATED_SERVICE_NAME_LENGTH) {
                Some(truncated) => truncated,
                None => {
                    return Err(ServiceAccountManagerError::InvalidServiceName {
                        given: clean_name,
                        reason: "Could not truncate".into(),
                    });
                }
            };

            for attempt_number in 0u32.. {
                let attempt_entry = if attempt_number == 0 {
                    clean_name.clone()
                } else {
                    format!("{clean_name}\0{attempt_number}")
                };
                let hash = hash::fnv1a(&attempt_entry);
                let candidate = format!("{truncated}.{hash:016x}");

                let search_result =
                    system_name_to_display_name.try_get_parsed(&candidate, parse_string)?;
                if search_result.is_none() || search_result.unwrap() == display_name {
                    return Ok(candidate.to_string());
                }
            }
            unreachable!()
        } else {
            return Ok(clean_name);
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Group {
    name: String,
    members: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ServiceDefinition {
    name: String,
    share_groups: HashMap<String, Group>,
}

impl std::fmt::Display for ServiceDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("{} service", self.name,))
    }
}

fn parse_service_definition(
    bytes: sled::IVec,
) -> Result<ServiceDefinition, ServiceAccountManagerError> {
    Ok(from_bytes(&bytes)?)
}

fn parse_string(bytes: sled::IVec) -> Result<String, ServiceAccountManagerError> {
    Ok(std::str::from_utf8(&bytes)?.to_string())
}

trait ParsedValueAccessor<TError> {
    fn get_parsed_with_message<TValue, TParser>(
        &self,
        key: &str,
        parser: TParser,
        message: &str,
    ) -> Result<TValue, ServiceAccountManagerError>
    where
        TParser: Fn(sled::IVec) -> Result<TValue, ServiceAccountManagerError>;

    fn get_parsed<TValue, TParser>(&self, key: &str, parser: TParser) -> Result<TValue, TError>
    where
        TParser: Fn(sled::IVec) -> Result<TValue, TError>;

    fn try_get_parsed<TValue, TParser>(
        &self,
        key: &str,
        parser: TParser,
    ) -> Result<Option<TValue>, TError>
    where
        TParser: Fn(sled::IVec) -> Result<TValue, TError>;

    fn has_key(&self, key: &str) -> Result<bool, TError>;
    fn insert<TValue>(&self, key: &str, value: &TValue) -> Result<(), ServiceAccountManagerError>
    where
        TValue: serde::Serialize;

    fn remove(&self, key: &str) -> Result<bool, ServiceAccountManagerError>;
}

impl ParsedValueAccessor<ServiceAccountManagerError> for Tree {
    fn get_parsed<TValue, TParser>(
        &self,
        key: &str,
        parser: TParser,
    ) -> Result<TValue, ServiceAccountManagerError>
    where
        TParser: Fn(sled::IVec) -> Result<TValue, ServiceAccountManagerError>,
    {
        self.get_parsed_with_message(key, parser, &format!("No entry for '{key}'"))
    }

    fn get_parsed_with_message<TValue, TParser>(
        &self,
        key: &str,
        parser: TParser,
        message: &str,
    ) -> Result<TValue, ServiceAccountManagerError>
    where
        TParser: Fn(sled::IVec) -> Result<TValue, ServiceAccountManagerError>,
    {
        if let Some(result) = self.try_get_parsed(key, parser)? {
            Ok(result)
        } else {
            Err(ServiceAccountManagerError::MissingDefinition(
                message.to_string(),
            ))
        }
    }

    fn try_get_parsed<TValue, TParser>(
        &self,
        key: &str,
        parser: TParser,
    ) -> Result<Option<TValue>, ServiceAccountManagerError>
    where
        TParser: Fn(sled::IVec) -> Result<TValue, ServiceAccountManagerError>,
    {
        match self.get(key)? {
            Some(bytes) => Ok(Some(parser(bytes)?)),
            None => Ok(None),
        }
    }

    fn has_key(&self, key: &str) -> Result<bool, ServiceAccountManagerError> {
        Ok(self.get(key)?.is_some())
    }

    fn insert<TValue>(&self, key: &str, value: &TValue) -> Result<(), ServiceAccountManagerError>
    where
        TValue: serde::Serialize,
    {
        self.insert(key, to_allocvec(value)?)?;
        Ok(())
    }

    fn remove(&self, key: &str) -> Result<bool, ServiceAccountManagerError> {
        match self.remove(key)? {
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }
}

struct Context {
    credentials: Arc<dyn Credentials>,
    service_name_to_definition: Tree,

    group_system_name_to_service_name: Tree,
    user_system_name_to_service_name: Tree,

    group_system_name_to_display_name: Tree,
    user_system_name_to_display_name: Tree,

    requested_service_name: String,
}

struct AccountSteps {
    user_display_name: String,
    user_system_name: String,
    primary_group_display_name: String,
    primary_group_system_name: String,

    steps: Vec<Step>,
}

struct PlanFactory {
    credentials: Arc<dyn Credentials>,
    service_name_to_definition: Tree,

    group_system_name_to_service_name: Tree,
    user_system_name_to_service_name: Tree,

    group_system_name_to_display_name: Tree,
    user_system_name_to_display_name: Tree,
}

impl PlanFactory {
    fn new(
        credentials: Arc<dyn Credentials>,
        service_name_to_definition: Tree,
        group_system_name_to_service_name: Tree,
        user_system_name_to_service_name: Tree,
        group_system_name_to_display_name: Tree,
        user_system_name_to_display_name: Tree,
    ) -> Self {
        Self {
            credentials,
            service_name_to_definition,
            group_system_name_to_service_name,
            user_system_name_to_service_name,
            group_system_name_to_display_name,
            user_system_name_to_display_name,
        }
    }

    fn create(
        &self,
        requested_service_name: &str,
    ) -> Result<Vec<Step>, ServiceAccountManagerError> {
        let service_definition = self.service_name_to_definition.get_parsed_with_message(
            requested_service_name,
            parse_service_definition,
            &format!("Could not find service definition '{requested_service_name}'"),
        )?;

        let mut result = Vec::new();

        let mut user_steps = self.ensure_service_account(&service_definition.name)?;
        let mut share_group_steps = self.ensure_share_groups(
            &service_definition.name,
            &user_steps.user_system_name,
            &service_definition.share_groups,
        )?;

        result.append(&mut user_steps.steps);
        result.append(&mut share_group_steps);

        return Ok(result);
    }

    fn ensure_service_account(
        &self,
        service_name: &str,
    ) -> Result<AccountSteps, ServiceAccountManagerError> {
        let (user_display_name, user_system_name) =
            Self::create_service_account_display_and_system_names(
                service_name,
                &self.user_system_name_to_display_name,
            )?;
        let (primary_group_display_name, primary_group_system_name) =
            Self::create_service_account_display_and_system_names(
                service_name,
                &self.group_system_name_to_display_name,
            )?;

        let user_exists = self.credentials.user_exists(&user_system_name);

        let primary_group_exists = self.credentials.group_exists(&primary_group_system_name);
        let mut result = Vec::new();

        if !user_exists && !primary_group_exists {
            push_step(
                &mut result,
                CreateGroup::for_service_primary_group(
                    &primary_group_display_name,
                    &primary_group_system_name,
                ),
            );

            push_step(
                &mut result,
                CreateUser::for_service_user(
                    &user_display_name,
                    &user_system_name,
                    &primary_group_system_name,
                ),
            );
        }

        if !user_exists && primary_group_exists {
            push_step(
                &mut result,
                CreateUser::for_service_user(
                    &user_display_name,
                    &user_system_name,
                    &primary_group_system_name,
                ),
            );
        }

        if user_exists && !primary_group_exists {
            push_step(
                &mut result,
                CreateGroup::for_service_primary_group(
                    &primary_group_display_name,
                    &primary_group_system_name,
                ),
            );

            push_step(
                &mut result,
                SetPrimayGroup::for_service_user(
                    &user_display_name,
                    &user_system_name,
                    &primary_group_display_name,
                    &primary_group_system_name,
                ),
            );
        }

        let user_primary_gid = self.credentials.get_primary_group(&user_system_name)?;
        let primary_group_id = match self.credentials.get_group_id(&primary_group_system_name) {
            Some(gid) => gid,
            None => {
                return Err(ServiceAccountManagerError::NotFound {
                    name: primary_group_display_name,
                    details: "The group exists, but could not determine the gid".to_string(),
                });
            }
        };

        if user_primary_gid != primary_group_id {
            push_step(
                &mut result,
                SetPrimayGroup::for_service_user(
                    &user_display_name,
                    &user_system_name,
                    &primary_group_display_name,
                    &primary_group_system_name,
                ),
            );
        }

        let (user_display_name_associated, user_system_name_associated) = Self::is_associated(
            &user_display_name,
            &user_system_name,
            service_name,
            &self.user_system_name_to_display_name,
            &self.user_system_name_to_service_name,
        )?;

        if !user_display_name_associated {
            push_step(
                &mut result,
                AssociateUserDisplayAndSystemNames::with_default_description(
                    &user_display_name,
                    &user_system_name,
                ),
            );
        }

        if !user_system_name_associated {
            push_step(
                &mut result,
                AssociateUserSystemNameAndService::with_default_description(
                    &user_system_name,
                    service_name,
                ),
            );
        }

        let (group_display_name_associated, group_system_name_associated) = Self::is_associated(
            &primary_group_display_name,
            &primary_group_system_name,
            service_name,
            &self.group_system_name_to_display_name,
            &self.group_system_name_to_service_name,
        )?;

        if !group_display_name_associated {
            push_step(
                &mut result,
                AssociateGroupSystemNameAndDisplayName::with_default_description(
                    &primary_group_display_name,
                    &primary_group_system_name,
                ),
            );
        }

        if !group_system_name_associated {
            push_step(
                &mut result,
                AssociateGroupSystemNameAndService::with_default_description(
                    &primary_group_system_name,
                    service_name,
                ),
            );
        }

        Ok(AccountSteps {
            user_display_name,
            user_system_name,
            primary_group_display_name,
            primary_group_system_name,
            steps: result,
        })
    }

    fn is_associated(
        display_name: &str,
        system_name: &str,
        service_name: &str,
        system_to_display: &Tree,
        system_to_service: &Tree,
    ) -> Result<(bool, bool), ServiceAccountManagerError> {
        let display_associated = match system_to_display
            .try_get_parsed(system_name, parse_string)?
        {
            Some(actual) => {
                if actual == display_name {
                    true
                } else {
                    return Err(ServiceAccountManagerError::ConfigurationMismatch(format!(
                        "The system name '{system_name}' was expected to be associated with the display name '{display_name}', but was instead assocaited with '{actual}'"
                    )));
                }
            }
            None => false,
        };

        let system_associated = match system_to_service
            .try_get_parsed(service_name, parse_string)?
        {
            Some(actual) => {
                if actual == service_name {
                    true
                } else {
                    return Err(ServiceAccountManagerError::ConfigurationMismatch(format!(
                        "The system name '{system_name}' was expected to be associated with the service '{service_name}', but was instead assocaited with '{actual}'"
                    )));
                }
            }
            None => false,
        };

        Ok((display_associated, system_associated))
    }

    fn create_service_account_display_and_system_names(
        service_name: &str,
        service_name_to_display_name: &Tree,
    ) -> Result<(String, String), ServiceAccountManagerError> {
        let display_name = names::create_account_display_name(service_name);
        let system_name = names::create_system_name(&display_name, service_name_to_display_name)?;
        Ok((display_name, system_name))
    }

    fn ensure_share_groups(
        &self,
        service_name: &str,
        service_account_user_system_name: &str,
        share_groups: &HashMap<String, Group>,
    ) -> Result<Vec<Step>, ServiceAccountManagerError> {
        let mut result = Vec::new();

        for share_group in share_groups.values() {
            let share_group_display_name =
                names::create_share_group_display_name(service_name, &share_group.name);

            let share_group_system_name = names::create_system_name(
                &share_group_display_name,
                &self.group_system_name_to_display_name,
            )?;

            if !self.credentials.group_exists(&share_group_system_name) {
                push_step(
                    &mut result,
                    CreateGroup::for_share(&share_group_display_name, &share_group_system_name),
                );
            }

            let (group_display_name_associated, group_system_name_associated) =
                Self::is_associated(
                    &share_group_display_name,
                    &share_group_system_name,
                    service_name,
                    &self.group_system_name_to_display_name,
                    &self.group_system_name_to_service_name,
                )?;

            if !group_display_name_associated {
                push_step(
                    &mut result,
                    AssociateGroupSystemNameAndDisplayName::with_default_description(
                        &share_group_display_name,
                        &share_group_system_name,
                    ),
                );
            }

            if !group_system_name_associated {
                push_step(
                    &mut result,
                    AssociateGroupSystemNameAndService::with_default_description(
                        &share_group_system_name,
                        service_name,
                    ),
                );
            }

            if !self
                .credentials
                .group_memberships(&share_group_system_name)
                .contains(&service_account_user_system_name.to_string())
            {
                push_step(
                    &mut result,
                    JoinGroup::with_default_description(
                        &service_account_user_system_name,
                        &share_group_system_name,
                    ),
                );
            }

            for member in share_group.members.iter() {
                let mut member_steps = self.ensure_service_account(&member)?;
                result.append(&mut member_steps.steps);

                if !self
                    .credentials
                    .group_memberships(&share_group_system_name)
                    .contains(&member_steps.user_system_name)
                {
                    push_step(
                        &mut result,
                        JoinGroup::with_default_description(
                            &member_steps.user_system_name,
                            &share_group_system_name,
                        ),
                    );
                }
            }
        }

        Ok(result)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CommandStatus {
    NotRun,
    Finished,
    RolledBack,
}
impl CommandStatus {
    fn can_skip(&self) -> bool {
        match self {
            CommandStatus::NotRun => false,
            CommandStatus::Finished | CommandStatus::RolledBack => true,
        }
    }
}

struct AssociateUserSystemNameAndService {
    description: String,
    user_name: String,
    service_name: String,
    status: CommandStatus,
}

impl AssociateUserSystemNameAndService {
    fn with_default_description(user_name: &str, service_name: &str) -> Self {
        Self::new(
            &format!("Associating user {user_name} with service {service_name}"),
            user_name,
            service_name,
        )
    }

    fn new(description: &str, user_name: &str, service_name: &str) -> Self {
        Self {
            description: description.to_string(),
            user_name: user_name.to_string(),
            service_name: service_name.to_string(),
            status: CommandStatus::NotRun,
        }
    }
}

impl std::fmt::Display for AssociateUserSystemNameAndService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("[{}]: {}", self.name(), self.description))
    }
}

impl Command<Context> for AssociateUserSystemNameAndService {
    type Error = ServiceAccountManagerError;

    fn name(&self) -> &str {
        "Associate user system name with service"
    }

    fn run(&mut self, logger: &dyn log::Logger, context: &mut Context) -> Result<(), Self::Error> {
        if self.status == CommandStatus::NotRun {
            logger.info(&self.description);
            context
                .group_system_name_to_service_name
                .insert(&self.user_name, to_allocvec(&self.service_name)?)?;
            self.status = CommandStatus::Finished;
        }
        Ok(())
    }

    fn rollback(
        &mut self,
        logger: &dyn log::Logger,
        context: &mut Context,
    ) -> Result<(), Self::Error> {
        if self.status == CommandStatus::Finished {
            logger.info(&format!(
                "Disassociating user {} from service {}",
                self.user_name, self.service_name
            ));
            context
                .group_system_name_to_service_name
                .remove(&self.user_name)?;

            self.status = CommandStatus::RolledBack
        }

        Ok(())
    }
}

struct AssociateGroupSystemNameAndDisplayName {
    description: String,
    group_system_name: String,
    group_display_name: String,
    status: CommandStatus,
}

impl AssociateGroupSystemNameAndDisplayName {
    fn with_default_description(group_system_name: &str, group_display_name: &str) -> Self {
        Self::new(
            &format!(
                "Associating group {group_system_name} with display name {group_display_name}"
            ),
            group_system_name,
            group_display_name,
        )
    }

    fn new(description: &str, group_system_name: &str, service_name: &str) -> Self {
        Self {
            description: description.to_string(),
            group_system_name: group_system_name.to_string(),
            group_display_name: service_name.to_string(),
            status: CommandStatus::NotRun,
        }
    }
}

impl std::fmt::Display for AssociateGroupSystemNameAndDisplayName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("[{}]: {}", self.name(), self.description))
    }
}

impl Command<Context> for AssociateGroupSystemNameAndDisplayName {
    type Error = ServiceAccountManagerError;

    fn name(&self) -> &str {
        "Associate group system sname with display name"
    }

    fn run(&mut self, logger: &dyn log::Logger, context: &mut Context) -> Result<(), Self::Error> {
        if self.status == CommandStatus::NotRun {
            logger.info(&self.description);
            context.group_system_name_to_display_name.insert(
                &self.group_system_name,
                to_allocvec(&self.group_display_name)?,
            )?;
            self.status = CommandStatus::Finished;
        }
        Ok(())
    }

    fn rollback(
        &mut self,
        logger: &dyn log::Logger,
        context: &mut Context,
    ) -> Result<(), Self::Error> {
        if self.status == CommandStatus::Finished {
            logger.info(&format!(
                "Disassociating group system name {} from display name {}",
                self.group_system_name, self.group_display_name
            ));
            context
                .group_system_name_to_display_name
                .remove(&self.group_system_name)?;

            self.status = CommandStatus::RolledBack
        }

        Ok(())
    }
}

struct AssociateGroupSystemNameAndService {
    description: String,
    group_system_name: String,
    service_name: String,
    status: CommandStatus,
}

impl AssociateGroupSystemNameAndService {
    fn with_default_description(group_system_name: &str, service_name: &str) -> Self {
        Self::new(
            &format!("Associating group {group_system_name} with service {service_name}"),
            group_system_name,
            service_name,
        )
    }

    fn new(description: &str, group_system_name: &str, service_name: &str) -> Self {
        Self {
            description: description.to_string(),
            group_system_name: group_system_name.to_string(),
            service_name: service_name.to_string(),
            status: CommandStatus::NotRun,
        }
    }
}

impl std::fmt::Display for AssociateGroupSystemNameAndService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("[{}]: {}", self.name(), self.description))
    }
}

impl Command<Context> for AssociateGroupSystemNameAndService {
    type Error = ServiceAccountManagerError;

    fn name(&self) -> &str {
        "Associate group system sname with service"
    }

    fn run(&mut self, logger: &dyn log::Logger, context: &mut Context) -> Result<(), Self::Error> {
        if self.status == CommandStatus::NotRun {
            logger.info(&self.description);
            context
                .group_system_name_to_service_name
                .insert(&self.group_system_name, to_allocvec(&self.service_name)?)?;
            self.status = CommandStatus::Finished;
        }
        Ok(())
    }

    fn rollback(
        &mut self,
        logger: &dyn log::Logger,
        context: &mut Context,
    ) -> Result<(), Self::Error> {
        if self.status == CommandStatus::Finished {
            logger.info(&format!(
                "Disassociating group {} from service {}",
                self.group_system_name, self.service_name
            ));
            context
                .group_system_name_to_service_name
                .remove(&self.group_system_name)?;

            self.status = CommandStatus::RolledBack
        }

        Ok(())
    }
}

struct CreateGroup {
    description: String,
    group_name: String,
    status: CommandStatus,
}

impl CreateGroup {
    fn for_share(group_display_name: &str, group_system_name: &str) -> Self {
        Self::new(
            &format!("Creating share group {group_display_name}"),
            group_system_name,
        )
    }

    fn for_service_primary_group(
        primary_group_display_name: &str,
        primary_group_system_name: &str,
    ) -> Self {
        Self::new(
            &format!("Creating primary group {primary_group_display_name}"),
            primary_group_system_name,
        )
    }

    fn with_default_description(group_name: &str) -> Self {
        Self::new(&format!("Creating group {group_name}"), group_name)
    }

    fn new(description: &str, group_name: &str) -> Self {
        Self {
            description: description.to_string(),
            group_name: group_name.to_string(),
            status: CommandStatus::NotRun,
        }
    }
}

impl std::fmt::Display for CreateGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("[{}]: {}", self.name(), self.description))
    }
}

impl Command<Context> for CreateGroup {
    type Error = ServiceAccountManagerError;

    fn name(&self) -> &str {
        "Create group"
    }

    fn run(&mut self, logger: &dyn log::Logger, context: &mut Context) -> Result<(), Self::Error> {
        if self.status == CommandStatus::NotRun {
            logger.info(&self.description);
            context.credentials.create_group(&self.group_name)?;
            self.status = CommandStatus::Finished;
        }
        Ok(())
    }

    fn rollback(
        &mut self,
        logger: &dyn log::Logger,
        context: &mut Context,
    ) -> Result<(), Self::Error> {
        if self.status == CommandStatus::Finished {
            logger.info(&format!("Removing group {}", self.group_name));
            context.credentials.delete_group(&self.group_name)?;
        }
        self.status = CommandStatus::RolledBack;

        Ok(())
    }

    fn skip(&self, context: &Context) -> bool {
        context.credentials.group_exists(&self.group_name)
    }
}

struct SetPrimayGroup {
    description: String,
    user_name: String,
    primary_group_name: String,
    old_pgid: Option<u32>,
    status: CommandStatus,
}

impl SetPrimayGroup {
    fn for_service_user(
        user_display_name: &str,
        user_system_name: &str,
        primary_group_display_name: &str,
        primary_group_system_name: &str,
    ) -> Self {
        Self::new(
            &format!(
                "Setting user '{user_display_name}' primary group to '{primary_group_display_name}'"
            ),
            user_system_name,
            primary_group_system_name,
        )
    }

    fn new(description: &str, user_name: &str, primary_group_name: &str) -> Self {
        Self {
            description: description.to_string(),
            user_name: user_name.to_string(),
            primary_group_name: primary_group_name.to_string(),
            old_pgid: None,
            status: CommandStatus::NotRun,
        }
    }
}

impl std::fmt::Display for SetPrimayGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("[{}]: {}", self.name(), self.description))
    }
}

impl Command<Context> for SetPrimayGroup {
    type Error = ServiceAccountManagerError;

    fn name(&self) -> &str {
        "Set primary group"
    }

    fn run(&mut self, logger: &dyn log::Logger, context: &mut Context) -> Result<(), Self::Error> {
        if self.status == CommandStatus::NotRun {
            logger.info(&self.description);
            self.old_pgid = Some(context.credentials.get_primary_group(&self.user_name)?);
            context
                .credentials
                .set_primary_group(&self.user_name, &self.primary_group_name)?;

            self.status = CommandStatus::Finished;
        }
        Ok(())
    }

    fn rollback(
        &mut self,
        logger: &dyn log::Logger,
        context: &mut Context,
    ) -> Result<(), Self::Error> {
        if self.status == CommandStatus::Finished
            && let Some(old_pgid) = self.old_pgid
        {
            logger.info(&format!("Restoring primary group id to {old_pgid}",));
            context
                .credentials
                .set_primary_group_id(&self.user_name, old_pgid)?;
        }
        self.status = CommandStatus::RolledBack;

        Ok(())
    }
}

struct JoinGroup {
    description: String,
    user_name: String,
    group_name: String,
    status: CommandStatus,
}

impl JoinGroup {
    fn with_default_description(user_name: &str, group_name: &str) -> Self {
        Self::new(
            &format!("Adding user {user_name} to group {group_name}"),
            user_name,
            group_name,
        )
    }

    fn new(description: &str, user_name: &str, group_name: &str) -> Self {
        Self {
            description: description.to_string(),
            user_name: user_name.to_string(),
            group_name: group_name.to_string(),
            status: CommandStatus::NotRun,
        }
    }
}

impl std::fmt::Display for JoinGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("[{}]: {}", self.name(), self.description))
    }
}

impl Command<Context> for JoinGroup {
    type Error = ServiceAccountManagerError;

    fn name(&self) -> &str {
        "Join group"
    }

    fn run(&mut self, logger: &dyn log::Logger, context: &mut Context) -> Result<(), Self::Error> {
        if self.status == CommandStatus::NotRun {
            logger.info(&self.description);
            context
                .credentials
                .join_group(&self.user_name, &self.group_name)?;

            self.status == CommandStatus::Finished;
        }
        Ok(())
    }

    fn rollback(
        &mut self,
        logger: &dyn log::Logger,
        context: &mut Context,
    ) -> Result<(), Self::Error> {
        if self.status == CommandStatus::Finished {
            logger.info(&format!(
                "Removing user {} from group {}",
                self.user_name, self.group_name
            ));
            context
                .credentials
                .leave_group(&self.user_name, &self.group_name)?;
        }
        self.status = CommandStatus::RolledBack;
        Ok(())
    }

    fn skip(&self, context: &Context) -> bool {
        let memberships = context.credentials.group_memberships(&self.user_name);
        memberships.contains(&self.group_name)
    }
}

struct CreateUser {
    description: String,
    user_name: String,
    group_name: String,
    status: CommandStatus,
}

impl CreateUser {
    fn for_service_user(
        user_display_name: &str,
        user_system_name: &str,
        primamry_group_system_name: &str,
    ) -> Self {
        Self::new(
            &format!("Creating service uesr {user_display_name}"),
            user_system_name,
            primamry_group_system_name,
        )
    }

    fn with_default_description(user_name: &str, group_name: &str) -> Self {
        Self::new(
            &format!("Creating user {user_name} with primary group {group_name}"),
            user_name,
            group_name,
        )
    }

    fn new(description: &str, user_name: &str, group_name: &str) -> Self {
        Self {
            description: description.to_string(),
            user_name: user_name.to_string(),
            group_name: group_name.to_string(),
            status: CommandStatus::NotRun,
        }
    }
}

impl std::fmt::Display for CreateUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("[{}]: {}", self.name(), self.description))
    }
}

impl Command<Context> for CreateUser {
    type Error = ServiceAccountManagerError;

    fn name(&self) -> &str {
        "Create user"
    }

    fn run(&mut self, logger: &dyn log::Logger, context: &mut Context) -> Result<(), Self::Error> {
        if self.status == CommandStatus::NotRun {
            logger.info(&self.description);
            context.credentials.create_user(
                &self.user_name,
                &self.group_name,
                vec![DOUGLAS_APP_GROUP.to_string()],
            )?;
            self.status = CommandStatus::Finished
        }

        Ok(())
    }

    fn rollback(
        &mut self,
        logger: &dyn log::Logger,
        context: &mut Context,
    ) -> Result<(), Self::Error> {
        if self.status == CommandStatus::Finished {
            for group_name in [&self.group_name, DOUGLAS_APP_GROUP] {
                logger.info(&format!(
                    "Removing user {} from group {group_name}",
                    self.user_name
                ));
                context
                    .credentials
                    .leave_group(&self.user_name, group_name)?;
            }

            logger.info(&format!("Deleting user {}", self.user_name));
            context.credentials.delete_user(&self.user_name)?;
        }

        self.status = CommandStatus::RolledBack;
        Ok(())
    }
}

impl std::fmt::Display for LeaveGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("[{}]: {}", self.name(), self.description))
    }
}

struct LeaveGroup {
    description: String,
    user_name: String,
    group_name: String,
    status: CommandStatus,
}

impl LeaveGroup {
    fn with_default_description(user_name: &str, group_name: &str) -> Self {
        Self::new(
            &format!("Removing user {user_name} from group {group_name}"),
            user_name,
            group_name,
        )
    }

    fn new(description: &str, user_name: &str, group_name: &str) -> Self {
        Self {
            description: description.to_string(),
            user_name: user_name.to_string(),
            group_name: group_name.to_string(),
            status: CommandStatus::NotRun,
        }
    }
}

impl Command<Context> for LeaveGroup {
    type Error = ServiceAccountManagerError;

    fn name(&self) -> &str {
        "Leave group"
    }

    fn run(&mut self, logger: &dyn log::Logger, context: &mut Context) -> Result<(), Self::Error> {
        if self.status == CommandStatus::NotRun {
            logger.info(&self.description);
            context
                .credentials
                .leave_group(&self.user_name, &self.group_name)?;
            self.status == CommandStatus::Finished;
        }
        Ok(())
    }

    fn rollback(
        &mut self,
        logger: &dyn log::Logger,
        context: &mut Context,
    ) -> Result<(), Self::Error> {
        if self.status == CommandStatus::Finished {
            logger.info(&format!(
                "Rejoining user {} to group {}",
                self.user_name, self.group_name
            ));
            context
                .credentials
                .join_group(&self.user_name, &self.group_name)?
        }
        self.status == CommandStatus::RolledBack;
        Ok(())
    }
}

struct AssociateUserDisplayAndSystemNames {
    description: String,
    user_display_name: String,
    user_system_name: String,
    status: CommandStatus,
}

impl AssociateUserDisplayAndSystemNames {
    fn with_default_description(user_display_name: &str, user_system_name: &str) -> Self {
        Self::new(
            &format!(
                "Associating user display name {user_display_name} with system name {user_system_name}"
            ),
            user_display_name,
            user_system_name,
        )
    }

    fn new(description: &str, user_display_name: &str, user_system_name: &str) -> Self {
        Self {
            description: description.to_string(),
            user_display_name: user_display_name.to_string(),
            user_system_name: user_system_name.to_string(),
            status: CommandStatus::NotRun,
        }
    }
}

impl std::fmt::Display for AssociateUserDisplayAndSystemNames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("[{}]: {}", self.name(), self.description))
    }
}

impl Command<Context> for AssociateUserDisplayAndSystemNames {
    type Error = ServiceAccountManagerError;

    fn name(&self) -> &str {
        "Associate user display name with user system name"
    }

    fn run(&mut self, logger: &dyn log::Logger, context: &mut Context) -> Result<(), Self::Error> {
        if self.status == CommandStatus::NotRun {
            logger.info(&self.description);
            context.user_system_name_to_display_name.insert(
                &self.user_system_name,
                to_allocvec(&self.user_display_name)?,
            )?;
            self.status = CommandStatus::Finished;
        }
        Ok(())
    }

    fn rollback(
        &mut self,
        logger: &dyn log::Logger,
        context: &mut Context,
    ) -> Result<(), Self::Error> {
        if self.status == CommandStatus::Finished {
            logger.info(&format!(
                "Disssociating user display name {} from system name {}",
                self.user_system_name, self.user_display_name
            ));
            context
                .group_system_name_to_service_name
                .remove(&self.user_system_name)?;

            self.status = CommandStatus::RolledBack
        }

        Ok(())
    }
}
