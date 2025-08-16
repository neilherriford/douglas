use super::{ClientErrorDisplay, token_validator::TokenValidator};
use crate::Response;
use crate::constants::DOUGLAS_GROUP;
use crate::encoding::safe_prefixed_credential_name;
use credentials::{Credentials, CredentialsError};
use log::Logger;
use std::sync::Arc;

pub(super) struct CreateCredentials {
    token: Arc<TokenValidator>,
    log: Arc<dyn Logger + Sync + Send + 'static>,
    credentials: Arc<dyn Credentials + Sync + Send + 'static>,
}
impl CreateCredentials {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send + 'static>,
        token_validator: Arc<TokenValidator>,
        credentials: Arc<dyn Credentials + Sync + Send + 'static>,
    ) -> Self {
        Self {
            token: token_validator,
            log,
            credentials,
        }
    }

    pub fn create(&self, token: String, service_name: String) -> Response {
        let (user_name, group_name) = safe_prefixed_credential_name(&service_name);

        self.log
            .info(&format!("Creating credentials for {}", service_name));
        self.token.perform_if_valid(token, move || {
            or_log_and_return_error!(
                self.log => warn,
                self.credentials.create_group(&group_name)            );
            or_log_and_return_error!(
                self.log => warn,
                self.credentials.create_user(
                    &user_name,
                    &group_name,
                    vec![DOUGLAS_GROUP.to_string()],
                )
            );

            Response::CredentialsCreated {
                user: user_name.clone(),
                group: group_name.clone(),
            }
        })
    }
}

impl ClientErrorDisplay for CredentialsError {
    fn to_client_string(&self) -> String {
        "Could not create credentials".to_string()
    }
}

#[cfg(test)]
mod tests {
    mod create {
        use super::super::*;
        use credentials::MockCredentials;
        use file_system::MockFileReader;
        use log::MockLogger;
        use mockall::predicate;
        use std::path::Path;

        fn build(
            logger: Arc<MockLogger>,
            credentials: Arc<MockCredentials>,
            file_reader: Arc<MockFileReader>,
            token_path: &Path,
        ) -> CreateCredentials {
            let token_validator = Arc::new(TokenValidator::new(
                logger.clone(),
                file_reader.clone(),
                token_path,
            ));

            CreateCredentials::new(logger.clone(), token_validator.clone(), credentials.clone())
        }

        fn given_group_created(credentials: &mut MockCredentials, expected_group_name: &str) {
            let expected_group_name = expected_group_name.to_string();
            credentials
                .expect_create_group()
                .with(predicate::eq(expected_group_name.clone()))
                .returning(|_| Ok(()));
        }

        #[test]
        fn should_validate_token() {
            let mut logger = MockLogger::new();
            let credentials = MockCredentials::new();
            let mut file_reader = MockFileReader::new();
            let token_path = Path::new("/tmp/token");

            logger.expect_info().return_const(());
            logger
                .expect_warn()
                .with(predicate::eq("Invalid token"))
                .return_const(());

            file_reader.given_can_read_all_with_contents("/tmp/token", "token");

            let actual = build(
                Arc::new(logger),
                Arc::new(credentials),
                Arc::new(file_reader),
                &token_path,
            )
            .create("oops".to_string(), "foo".to_string());

            assert!(matches!(actual, Response::InvalidToken));
        }

        #[test]
        fn should_return_error_if_group_failed() {
            let mut logger = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let mut file_reader = MockFileReader::new();
            let token_path = Path::new("/tmp/token");

            logger.expect_info().return_const(());
            logger.expect_warn().return_const(());

            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            credentials
                .expect_create_group()
                .with(predicate::eq("doug-foo"))
                .returning(|_| Err(CredentialsError::InvalidName));

            let actual = build(
                Arc::new(logger),
                Arc::new(credentials),
                Arc::new(file_reader),
                &token_path,
            )
            .create("token".to_string(), "foo".to_string());

            assert!(matches!(actual, Response::Error(_)));
        }

        #[test]
        fn should_return_error_if_user_failed() {
            let mut logger = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let mut file_reader = MockFileReader::new();
            let token_path = Path::new("/tmp/token");

            logger.expect_info().return_const(());
            logger.expect_warn().return_const(());

            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            given_group_created(&mut credentials, "doug-foo");
            credentials
                .expect_create_user()
                .with(
                    predicate::eq("doug-foo"),
                    predicate::eq("doug-foo"),
                    predicate::eq(vec!["douglas".to_string()]),
                )
                .returning(|_, _, _| Err(CredentialsError::InvalidName));

            let actual = build(
                Arc::new(logger),
                Arc::new(credentials),
                Arc::new(file_reader),
                &token_path,
            )
            .create("token".to_string(), "foo".to_string());

            assert!(matches!(actual, Response::Error(_)));
        }

        #[test]
        fn should_create() {
            let mut logger = MockLogger::new();
            let mut credentials = MockCredentials::new();
            let mut file_reader = MockFileReader::new();
            let token_path = Path::new("/tmp/token");

            logger.expect_info().return_const(());
            logger.expect_warn().return_const(());

            file_reader.given_can_read_all_with_contents("/tmp/token", "token");
            given_group_created(&mut credentials, "doug-foo");
            credentials
                .expect_create_user()
                .with(
                    predicate::eq("doug-foo"),
                    predicate::eq("doug-foo"),
                    predicate::eq(vec!["douglas".to_string()]),
                )
                .returning(|_, _, _| Ok(()));

            let actual = build(
                Arc::new(logger),
                Arc::new(credentials),
                Arc::new(file_reader),
                &token_path,
            )
            .create("token".to_string(), "foo".to_string());

            assert!(matches!(
                    actual,
                    Response::CredentialsCreated {
                        user,
                        group
                    } if user =="doug-foo".to_string() && group == "doug-foo".to_string()));
        }
    }
}
