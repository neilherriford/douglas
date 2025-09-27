use crate::queries::{LocalQueries, Queries};
use crate::{Credentials, CredentialsError};
use os::Os;
use std::sync::Arc;

pub(crate) struct LinuxCredentials {
    os: Arc<dyn Os + Sync + Send + 'static>,
    queries: Box<dyn Queries + Sync + Send + 'static>,
}

impl LinuxCredentials {
    #![allow(dead_code)]
    pub fn new(os: Arc<dyn Os + Sync + Send + 'static>) -> Self {
        Self {
            os: os.clone(),
            queries: Box::new(LocalQueries::new()),
        }
    }

    fn add_to_group(&self, user_name: &str, group_name: &str) -> Result<(), CredentialsError> {
        if self.group_exists(group_name) {
            Ok(self.os.execute(
                "usermod",
                vec![
                    "--append".to_string(),
                    "--groups".to_string(),
                    group_name.to_string(),
                    user_name.to_string(),
                ],
            )?)
        } else {
            Err(CredentialsError::GroupNotFoundError {
                name: group_name.to_string(),
            })
        }
    }
}

impl Credentials for LinuxCredentials {
    fn is_root(&self) -> bool {
        self.queries.is_root()
    }

    fn create_user(
        &self,
        name: &str,
        primary_group_name: &str,
        group_names: Vec<String>,
    ) -> Result<(), CredentialsError> {
        if !self.user_exists(name) {
            self.os.execute(
                "useradd",
                vec![
                    "--system".to_string(),
                    "--no-create-home".to_string(),
                    "--gid".to_string(),
                    primary_group_name.to_string(),
                    "--home".to_string(),
                    "/nonexistent".to_string(),
                    "--shell".to_string(),
                    "/usr/sbin/nologin".to_string(),
                    name.to_string(),
                ],
            )?;
        }

        let existing = self.group_memberships(name);
        for group_name in group_names.iter().filter(|name| !existing.contains(name)) {
            self.add_to_group(name, group_name)?;
        }

        Ok(())
    }

    fn user_exists(&self, name: &str) -> bool {
        self.queries.user_exists(name)
    }

    fn delete_user(&self, name: &str) -> Result<(), CredentialsError> {
        if !self.user_exists(name) {
            return Ok(());
        }

        self.os.execute("userdel", vec![name.to_string()])?;
        Ok(())
    }

    fn create_group(&self, name: &str) -> Result<(), CredentialsError> {
        if self.group_exists(name) {
            return Ok(());
        }

        self.os.execute("groupadd", vec![name.to_string()])?;
        Ok(())
    }

    fn group_exists(&self, name: &str) -> bool {
        self.queries.group_exists(name)
    }

    fn group_memberships(&self, name: &str) -> Vec<String> {
        self.queries.group_memberships(name)
    }

    fn get_group_id(&self, name: &str) -> Option<u32> {
        self.queries.get_group_id(name)
    }

    fn delete_group(&self, name: &str) -> Result<(), CredentialsError> {
        if !self.group_exists(name) {
            return Ok(());
        }

        self.os.execute("groupdel", vec![name.to_string()])?;
        Ok(())
    }

    fn list_users(&self) -> Result<Vec<String>, CredentialsError> {
        let output = self
            .os
            .execute_with_output("getent", vec!["passwd".to_string()])?;

        if let Ok(output) = String::from_utf8(output.stdout) {
            Ok(output
                .split("\n")
                .map(|line| {
                    if let Some((user, _)) = line.split_once(":") {
                        user.to_string()
                    } else {
                        line.to_string()
                    }
                })
                .collect())
        } else {
            Err(CredentialsError::GeneralError(
                "Could not list users".to_string(),
            ))
        }
    }

    fn join_group(&self, user_name: &str, group_name: &str) -> Result<(), CredentialsError> {
        self.add_to_group(user_name, group_name)
    }
}

#[cfg(test)]
mod tests {
    mod is_root {
        use super::super::*;
        use crate::queries::MockQueries;
        use os::MockOs;
        use std::sync::Arc;

        #[test]
        fn should_determine_is_root() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries.expect_is_root().return_const(false);

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .is_root();

            assert!(!actual);
        }
    }

    mod create_user {
        use super::super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::{MockOs, OsError};

        #[test]
        fn should_not_recreate_existing_user() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);
            queries
                .expect_group_memberships()
                .with(predicate::eq("foo"))
                .returning(|_| vec!["bar".to_string(), "baz".to_string()]);

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec!["baz".to_string()]);

            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_add_existing_user_to_new_group() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);
            queries
                .expect_group_memberships()
                .with(predicate::eq("foo"))
                .returning(|_| vec!["bar".to_string()]);
            queries
                .expect_group_exists()
                .with(predicate::eq("baz"))
                .return_const(true);

            os.expect_execute()
                .with(
                    predicate::eq("usermod"),
                    predicate::eq(vec![
                        "--append".to_string(),
                        "--groups".to_string(),
                        "baz".to_string(),
                        "foo".to_string(),
                    ]),
                )
                .returning(|_, _| Ok(()));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec!["baz".to_string()]);
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_err_if_group_does_not_exist() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);
            queries
                .expect_group_memberships()
                .with(predicate::eq("foo"))
                .returning(|_| vec!["bar".to_string()]);
            queries
                .expect_group_exists()
                .with(predicate::eq("baz"))
                .return_const(false);

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec!["baz".to_string()]);

            assert!(
                matches!(actual, Err(CredentialsError::GroupNotFoundError { name }) if name == "baz" )
            );
        }

        #[test]
        fn should_error_if_user_could_not_be_created() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);
            os.expect_execute()
                .with(
                    predicate::eq("useradd"),
                    predicate::eq(vec![
                        "--system".to_string(),
                        "--no-create-home".to_string(),
                        "--gid".to_string(),
                        "bar".to_string(),
                        "--home".to_string(),
                        "/nonexistent".to_string(),
                        "--shell".to_string(),
                        "/usr/sbin/nologin".to_string(),
                        "foo".to_string(),
                    ]),
                )
                .returning(move |_, _| Err(OsError::IoError(std::io::Error::other("oops"))));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec!["baz".to_string()]);

            assert!(matches!(actual, Err(CredentialsError::IoError(_))));
        }

        #[test]
        fn should_error_if_could_not_add_group() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);
            os.expect_execute()
                .with(
                    predicate::eq("useradd"),
                    predicate::eq(vec![
                        "--system".to_string(),
                        "--no-create-home".to_string(),
                        "--gid".to_string(),
                        "bar".to_string(),
                        "--home".to_string(),
                        "/nonexistent".to_string(),
                        "--shell".to_string(),
                        "/usr/sbin/nologin".to_string(),
                        "foo".to_string(),
                    ]),
                )
                .returning(move |_, _| Ok(()));
            queries
                .expect_group_memberships()
                .with(predicate::eq("foo"))
                .returning(|_| vec!["bar".to_string()]);
            queries
                .expect_group_exists()
                .with(predicate::eq("baz"))
                .return_const(true);
            os.expect_execute()
                .with(
                    predicate::eq("usermod"),
                    predicate::eq(vec![
                        "--append".to_string(),
                        "--groups".to_string(),
                        "baz".to_string(),
                        "foo".to_string(),
                    ]),
                )
                .returning(|_, _| Err(OsError::IoError(std::io::Error::other("oops"))));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec!["baz".to_string()]);

            assert!(matches!(actual, Err(CredentialsError::IoError(_))));
        }

        #[test]
        fn should_add_create_user() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);
            os.expect_execute()
                .with(
                    predicate::eq("useradd"),
                    predicate::eq(vec![
                        "--system".to_string(),
                        "--no-create-home".to_string(),
                        "--gid".to_string(),
                        "bar".to_string(),
                        "--home".to_string(),
                        "/nonexistent".to_string(),
                        "--shell".to_string(),
                        "/usr/sbin/nologin".to_string(),
                        "foo".to_string(),
                    ]),
                )
                .returning(move |_, _| Ok(()));
            queries
                .expect_group_memberships()
                .with(predicate::eq("foo"))
                .returning(|_| vec!["bar".to_string()]);
            queries
                .expect_group_exists()
                .with(predicate::eq("baz"))
                .return_const(true);
            os.expect_execute()
                .with(
                    predicate::eq("usermod"),
                    predicate::eq(vec![
                        "--append".to_string(),
                        "--groups".to_string(),
                        "baz".to_string(),
                        "foo".to_string(),
                    ]),
                )
                .returning(|_, _| Ok(()));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec!["baz".to_string()]);

            assert!(matches!(actual, Ok(())));
        }
    }

    mod user_exists {
        use super::super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::MockOs;
        use std::sync::Arc;

        #[test]
        fn should_determine_user_exists() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .user_exists("foo");

            assert!(!actual);
        }
    }

    mod delete_user {
        use super::super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::{MockOs, OsError};
        use std::sync::Arc;

        #[test]
        fn should_nop_if_user_does_not_exist() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .delete_user("foo");

            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_error_if_del_user_fails() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_execute()
                .with(
                    predicate::eq("userdel"),
                    predicate::eq(vec!["foo".to_string()]),
                )
                .returning(|_, _| Err(OsError::IoError(std::io::Error::other("oops"))));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .delete_user("foo");

            assert!(matches!(actual, Err(CredentialsError::IoError(_))));
        }

        #[test]
        fn should_delete_user() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_execute()
                .with(
                    predicate::eq("userdel"),
                    predicate::eq(vec!["foo".to_string()]),
                )
                .returning(|_, _| Ok(()));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .delete_user("foo");

            assert!(matches!(actual, Ok(())));
        }
    }

    mod create_group {
        use super::super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::{MockOs, OsError};
        use std::sync::Arc;

        #[test]
        fn should_not_recreate_existing_group() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_group("foo");

            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_return_error_if_could_not_create_group() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            os.expect_execute()
                .withf(move |command, args| {
                    command == "groupadd" && *args == vec!["foo".to_string()]
                })
                .times(1)
                .return_once(move |_, _| Err(OsError::IoError(std::io::Error::other("oops"))));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_group("foo");
            assert!(matches!(actual, Err(CredentialsError::IoError(_))));
        }

        #[test]
        fn should_create_group() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            os.expect_execute()
                .withf(move |command, args| {
                    command == "groupadd" && *args == vec!["foo".to_string()]
                })
                .times(1)
                .return_once(move |_, _| Ok(()));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_group("foo");
            assert!(matches!(actual, Ok(())));
        }
    }

    mod group_exists {
        use super::super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::MockOs;
        use std::sync::Arc;

        #[test]
        fn should_determine_group_exists() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .group_exists("foo");

            assert!(!actual);
        }
    }

    mod group_memberships {
        use super::super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::MockOs;
        use std::sync::Arc;

        #[test]
        fn should_determine_group_memberships() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_memberships()
                .with(predicate::eq("foo"))
                .returning(|_| vec!["bar".to_string(), "baz".to_string()]);

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .group_memberships("foo");

            assert_eq!(vec!["bar".to_string(), "baz".to_string()], actual);
        }
    }

    mod get_group_id {
        use super::super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::MockOs;
        use std::sync::Arc;

        #[test]
        fn should_determine_get_group_id() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_get_group_id()
                .with(predicate::eq("foo"))
                .returning(|_| Some(123));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .get_group_id("foo");

            assert_eq!(Some(123), actual);
        }
    }

    mod delete_group {
        use super::super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::MockOs;
        use std::sync::Arc;

        #[test]
        fn should_ignore_non_existant_group() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .delete_group("foo");
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_return_err_if_could_not_delete_group() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_execute()
                .with(
                    predicate::eq("groupdel"),
                    predicate::eq(vec!["foo".to_string()]),
                )
                .returning(move |_, _| Ok(()));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .delete_group("foo");
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_delete_group() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_execute()
                .with(
                    predicate::eq("groupdel"),
                    predicate::eq(vec!["foo".to_string()]),
                )
                .returning(move |_, _| Ok(()));

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .delete_group("foo");
            assert!(matches!(actual, Ok(())));
        }
    }

    mod list_users {
        use crate::{
            Credentials, CredentialsError, linux_credentials::LinuxCredentials,
            queries::MockQueries,
        };
        use mockall::predicate;
        use os::{MockOs, OsError};
        use std::sync::Arc;

        #[test]
        fn should_error_if_command_fails() {
            let mut os = MockOs::new();
            let queries = MockQueries::new();

            os.expect_execute_with_output()
                .with(
                    predicate::eq("getent"),
                    predicate::eq(vec!["passwd".to_string()]),
                )
                .returning(|_, _| Err(OsError::IoError(std::io::Error::other("oops"))));
            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .list_users();

            assert!(matches!(actual, Err(CredentialsError::IoError(_))));
        }

        #[test]
        fn should_list_users() {
            let mut os = MockOs::new();
            let queries = MockQueries::new();

            os.expect_execute_with_output_for(
                "getent",
                vec!["passwd"],
                "foo:bar:baz\nbar:baz:qux",
                "",
                0,
            );

            let actual = LinuxCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .list_users();

            assert!(
                matches!(actual, Ok(users) if users == vec!["foo".to_string(), "bar".to_string()])
            );
        }
    }
}
