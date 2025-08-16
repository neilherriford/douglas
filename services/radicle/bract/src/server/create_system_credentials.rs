use crate::constants::{DOUGLAS_GROUP, RADICLE_GROUP, RADICLE_USER};
use credentials::{Credentials, CredentialsError};
use log::Logger;
use std::sync::Arc;

pub(super) struct CreateSystemCredentials {
    log: Arc<dyn Logger + Sync + Send + 'static>,
    credentials: Arc<dyn Credentials>,
}

impl CreateSystemCredentials {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send + 'static>,
        credentials: Arc<dyn Credentials + Sync + Send + 'static>,
    ) -> Self {
        Self { log, credentials }
    }

    fn create_group(&self, name: &str) -> Result<(), CredentialsError> {
        self.log.info(&format!("Creating '{}' group", name));
        self.credentials.create_group(name)
    }

    pub fn create(&self) -> Result<(), CredentialsError> {
        self.create_group(DOUGLAS_GROUP)?;
        self.create_group(RADICLE_GROUP)?;

        self.log.info(&format!("creating '{}' user", RADICLE_USER));
        self.credentials
            .create_user(RADICLE_USER, RADICLE_GROUP, vec![DOUGLAS_GROUP.to_string()])
    }
}

#[cfg(test)]
mod tests {
    mod create_system_credentials {
        use super::super::CreateSystemCredentials;
        use credentials::{CredentialsError, MockCredentials};
        use log::MockLogger;
        use mockall::predicate;
        use std::sync::Arc;

        #[test]
        fn should_err_if_douglas_group_failed() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();

            log.expect_info().return_const(());
            credentials
                .expect_create_group()
                .with(predicate::eq("douglas"))
                .returning(|_| Err(CredentialsError::InvalidName));

            let actual =
                CreateSystemCredentials::new(Arc::new(log), Arc::new(credentials)).create();

            assert!(matches!(actual, Err(CredentialsError::InvalidName)));
        }

        #[test]
        fn should_err_if_radicle_group_failed() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();

            log.expect_info().return_const(());
            credentials
                .expect_create_group()
                .with(predicate::eq("douglas"))
                .returning(|_| Ok(()));

            credentials
                .expect_create_group()
                .with(predicate::eq("doug-radicle"))
                .returning(|_| Err(CredentialsError::InvalidName));

            let actual =
                CreateSystemCredentials::new(Arc::new(log), Arc::new(credentials)).create();

            assert!(matches!(actual, Err(CredentialsError::InvalidName)));
        }

        #[test]
        fn should_err_if_radicle_user_failed() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();

            log.expect_info().return_const(());
            credentials
                .expect_create_group()
                .with(predicate::eq("douglas"))
                .returning(|_| Ok(()));

            credentials
                .expect_create_group()
                .with(predicate::eq("doug-radicle"))
                .returning(|_| Ok(()));

            credentials
                .expect_create_user()
                .with(
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                    predicate::eq(vec!["douglas".to_string()]),
                )
                .returning(|_, _, _| Err(CredentialsError::InvalidName));

            let actual =
                CreateSystemCredentials::new(Arc::new(log), Arc::new(credentials)).create();

            assert!(matches!(actual, Err(CredentialsError::InvalidName)));
        }

        #[test]
        fn should_create() {
            let mut log = MockLogger::new();
            let mut credentials = MockCredentials::new();

            log.expect_info().return_const(());
            credentials
                .expect_create_group()
                .with(predicate::eq("douglas"))
                .returning(|_| Ok(()));

            credentials
                .expect_create_group()
                .with(predicate::eq("doug-radicle"))
                .returning(|_| Ok(()));

            credentials
                .expect_create_user()
                .with(
                    predicate::eq("doug-radicle"),
                    predicate::eq("doug-radicle"),
                    predicate::eq(vec!["douglas".to_string()]),
                )
                .returning(|_, _, _| Ok(()));

            let actual =
                CreateSystemCredentials::new(Arc::new(log), Arc::new(credentials)).create();

            assert!(matches!(actual, Ok(())));
        }
    }
}
