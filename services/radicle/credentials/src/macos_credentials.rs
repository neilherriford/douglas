use crate::queries::{LocalQueries, Queries};
use crate::{Credentials, CredentialsError};
use os::Os;
use std::process::Output;
use std::sync::Arc;

#[derive(PartialEq)]
enum ObjectKind {
    User,
    Group,
}

impl std::fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            ObjectKind::User => "Users".to_string(),
            ObjectKind::Group => "Groups".to_string(),
        };

        write!(f, "{}", value)
    }
}

#[derive(PartialEq)]
enum Operation {
    Create,
    Delete,
    List,
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Operation::Create => "-create".to_string(),
            Operation::Delete => "-delete".to_string(),
            Operation::List => "-list".to_string(),
        };

        write!(f, "{}", value)
    }
}

#[derive(PartialEq)]
enum ObjectAttribute {
    GroupMembership,
    NFSHomeDirectory,
    PrimaryGroupID,
    UniqueID,
    UserShell,
}

impl std::fmt::Display for ObjectAttribute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            ObjectAttribute::GroupMembership => "GroupMembership".to_string(),
            ObjectAttribute::NFSHomeDirectory => "NFSHomeDirectory".to_string(),
            ObjectAttribute::PrimaryGroupID => "PrimaryGroupID".to_string(),
            ObjectAttribute::UniqueID => "UniqueID".to_string(),
            ObjectAttribute::UserShell => "UserShell".to_string(),
        };

        write!(f, "{}", value)
    }
}

pub(crate) struct MacOSCredentials {
    os: Arc<dyn Os + Sync + Send + 'static>,
    queries: Box<dyn Queries + Sync + Send + 'static>,
}

impl MacOSCredentials {
    #![allow(dead_code)]
    pub fn new(os: Arc<dyn Os + Sync + Send + 'static>) -> Self {
        Self {
            os: os.clone(),
            queries: Box::new(LocalQueries::new()),
        }
    }

    fn execute(
        &self,
        operation: Operation,
        path: &str,
        args: Vec<String>,
    ) -> Result<Output, CredentialsError> {
        let mut arguments = vec![".".to_string(), operation.to_string(), path.to_string()];
        for arg in &args {
            arguments.push(arg.to_string())
        }
        Ok(self.os.execute_with_output("dscl", arguments)?)
    }

    fn named_object_path(&self, name: &str, kind: ObjectKind) -> String {
        format!("/{}/{}", kind, name)
    }

    fn list(
        &self,
        kind: ObjectKind,
        attribute: ObjectAttribute,
    ) -> Result<Output, CredentialsError> {
        let path = format!("/{}", kind);
        self.execute(Operation::List, &path, vec![attribute.to_string()])
    }

    fn create_object(&self, name: &str, kind: ObjectKind) -> Result<(), CredentialsError> {
        let path = self.named_object_path(name, kind);
        self.execute(Operation::Create, &path, vec![])?;
        Ok(())
    }

    fn delete_object(&self, name: &str, kind: ObjectKind) -> Result<(), CredentialsError> {
        let path = self.named_object_path(name, kind);
        self.execute(Operation::Delete, &path, vec![])?;
        Ok(())
    }

    fn set_object_attribute(
        &self,
        name: &str,
        kind: ObjectKind,
        attribute: ObjectAttribute,
        value: String,
    ) -> Result<(), CredentialsError> {
        let path = self.named_object_path(name, kind);
        self.execute(Operation::Create, &path, vec![attribute.to_string(), value])?;
        Ok(())
    }

    fn get_first_unused_id(&self, kind: ObjectKind, min_id: u32) -> Result<u32, CredentialsError> {
        let id_attribute = match kind {
            ObjectKind::User => ObjectAttribute::UniqueID,
            ObjectKind::Group => ObjectAttribute::PrimaryGroupID,
        };

        let output = self.list(kind, id_attribute)?;
        let used = self.scrape_columnar_output::<u32>(&output, 1);
        let result = self.first_unused(used, min_id);
        Ok(result)
    }

    fn scrape_columnar_output<T>(&self, output: &Output, column: usize) -> Vec<T>
    where
        T: std::cmp::Ord + std::str::FromStr,
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut result: Vec<T> = stdout
            .lines()
            .filter_map(|line| line.split_whitespace().nth(column))
            .filter_map(|cell| cell.parse().ok())
            .collect();
        result.sort_unstable();
        result
    }

    fn first_unused(&self, used: Vec<u32>, starting: u32) -> u32 {
        let mut result = starting;
        while used.contains(&result) {
            result += 1;
        }

        return result;
    }

    fn add_to_group(&self, user_name: &str, group_name: &str) -> Result<(), CredentialsError> {
        if self.group_exists(&group_name) {
            self.set_object_attribute(
                &group_name,
                ObjectKind::Group,
                ObjectAttribute::GroupMembership,
                user_name.to_string(),
            )
        } else {
            Err(CredentialsError::GroupNotFoundError {
                name: group_name.to_string(),
            })
        }
    }
}

static MINIMUM_ID: u32 = 501;

impl Credentials for MacOSCredentials {
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
            let uid = self.get_first_unused_id(ObjectKind::User, MINIMUM_ID)?;

            let primary_gid = self.get_group_id(primary_group_name).ok_or(
                CredentialsError::GroupNotFoundError {
                    name: primary_group_name.to_string(),
                },
            )?;

            self.create_object(name, ObjectKind::User)?;
            for user_setting in [
                (ObjectAttribute::UniqueID, uid.to_string()),
                (ObjectAttribute::UserShell, "/usr/bin/false".to_string()),
                (ObjectAttribute::NFSHomeDirectory, "/var/empty".to_string()),
                (ObjectAttribute::PrimaryGroupID, primary_gid.to_string()),
            ] {
                self.set_object_attribute(name, ObjectKind::User, user_setting.0, user_setting.1)?;
            }
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

        self.delete_object(name, ObjectKind::User)?;
        Ok(())
    }

    fn create_group(&self, name: &str) -> Result<(), CredentialsError> {
        if self.group_exists(name) {
            return Ok(());
        }

        let gid = self.get_first_unused_id(ObjectKind::Group, MINIMUM_ID)?;
        self.create_object(name, ObjectKind::Group)?;
        self.set_object_attribute(
            name,
            ObjectKind::Group,
            ObjectAttribute::PrimaryGroupID,
            gid.to_string(),
        )?;

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
        self.delete_object(name, ObjectKind::Group)?;
        Ok(())
    }

    fn list_users(&self) -> Result<Vec<String>, CredentialsError> {
        let output = self.execute(Operation::List, "/Users", vec![])?;
        if let Ok(lines) = String::from_utf8(output.stdout) {
            Ok(lines
                .split("\n")
                .filter_map(|name| {
                    if name.len() == 0 {
                        None
                    } else {
                        Some(name.to_string())
                    }
                })
                .collect())
        } else {
            Err(CredentialsError::GeneralError(
                "Error listing users".to_string(),
            ))
        }
    }

    fn join_group(&self, user_name: &str, group_name: &str) -> Result<(), CredentialsError> {
        self.add_to_group(user_name, group_name)
    }
}

#[cfg(test)]
mod tests {
    static EMPTY_STDERR: &str = "";
    static STATUS_SUCCESS: i32 = 0;

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

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .is_root();

            assert_eq!(false, actual);
        }
    }

    mod create_user {
        use super::super::*;
        use super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::MockOs;

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
                .return_const(vec!["baz".to_string()]);

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec!["baz".to_string()]);

            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_err_if_new_group_does_not_exist() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            queries
                .expect_group_memberships()
                .with(predicate::eq("foo"))
                .return_const(vec!["baz".to_string()]);

            queries
                .expect_group_exists()
                .with(predicate::eq("qux"))
                .return_const(false);

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec!["qux".to_string()]);

            assert!(
                matches!(actual, Err(CredentialsError::GroupNotFoundError { name }) if name == "qux".to_string() )
            );
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
                .return_const(vec!["baz".to_string()]);

            queries
                .expect_group_exists()
                .with(predicate::eq("qux"))
                .return_const(true);

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![".", "-create", "/Groups/qux", "GroupMembership", "foo"],
            );

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec!["qux".to_string()]);
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_error_if_primary_group_not_found() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![".", "-list", "/Users", "UniqueID"],
            );

            queries
                .expect_get_group_id()
                .with(predicate::eq("bar"))
                .return_const(None);

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec![]);

            assert!(
                matches!(actual, Err(CredentialsError::GroupNotFoundError { name }) if name == "bar".to_string() )
            );
        }

        #[test]
        fn should_create_user() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            os.expect_execute_with_output_for(
                "dscl",
                vec![".", "-list", "/Users", "UniqueID"],
                r#"
bar        501
baz        502
qux        504
                "#,
                EMPTY_STDERR,
                STATUS_SUCCESS,
            );

            queries
                .expect_get_group_id()
                .with(predicate::eq("bar"))
                .return_const(Some(1234));

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![".", "-create", "/Users/foo"],
            );
            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![".", "-create", "/Users/foo", "UniqueID", "503"],
            );

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![".", "-create", "/Users/foo", "UserShell", "/usr/bin/false"],
            );

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".",
                    "-create",
                    "/Users/foo",
                    "NFSHomeDirectory",
                    "/var/empty",
                ],
            );

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![".", "-create", "/Users/foo", "PrimaryGroupID", "1234"],
            );

            queries
                .expect_group_memberships()
                .with(predicate::eq("foo"))
                .return_const(vec![]);

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_user("foo", "bar", vec![]);

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

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .user_exists("foo");

            assert_eq!(false, actual);
        }
    }

    mod delete_user {
        use super::super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::MockOs;

        #[test]
        fn should_ignore_non_exsiting_users() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .delete_user("foo");
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_delete_user() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![".", "-delete", "/Users/foo"],
            );

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .delete_user("foo");
            assert!(matches!(actual, Ok(())));
        }
    }

    mod create_group {
        use super::super::*;
        use super::*;
        use crate::queries::MockQueries;
        use mockall::predicate;
        use os::MockOs;

        #[test]
        fn should_not_create_existing_group() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .create_group("foo");

            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_create_group() {
            let mut os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            os.expect_execute_with_output_for(
                "dscl",
                vec![".", "-list", "/Groups", "PrimaryGroupID"],
                r#"
bar        501
baz        502
qux        504
                "#,
                EMPTY_STDERR,
                STATUS_SUCCESS,
            );

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![".", "-create", "/Groups/foo"],
            );

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![".", "-create", "/Groups/foo", "PrimaryGroupID", "503"],
            );

            let actual = MacOSCredentials {
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

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .group_exists("foo");

            assert_eq!(false, actual);
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

            let actual = MacOSCredentials {
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

            let actual = MacOSCredentials {
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
        use os::MockOs;

        use mockall::predicate;

        #[test]
        fn should_ignore_non_exsiting_groups() {
            let os = MockOs::new();
            let mut queries = MockQueries::new();

            queries
                .expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            let actual = MacOSCredentials {
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

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![".", "-delete", "/Groups/foo"],
            );

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .delete_group("foo");
            assert!(matches!(actual, Ok(())));
        }
    }

    mod list_users {
        use std::sync::Arc;

        use mockall::predicate;
        use os::{MockOs, OsError};

        use crate::{
            Credentials, CredentialsError, macos_credentials::MacOSCredentials,
            queries::MockQueries,
        };

        #[test]
        fn should_error_if_list_failed() {
            let mut os = MockOs::new();
            let queries = MockQueries::new();

            os.expect_execute_with_output()
                .with(
                    predicate::eq("dscl".to_string()),
                    predicate::eq(vec![
                        ".".to_string(),
                        "-list".to_string(),
                        "/Users".to_string(),
                    ]),
                )
                .returning(|_, _| Err(OsError::IoError(std::io::Error::other("oops"))));

            let actual = MacOSCredentials {
                os: Arc::new(os),
                queries: Box::new(queries),
            }
            .list_users();

            assert!(matches!(actual, Err(CredentialsError::IoError(_))));
        }

        #[test]
        fn should_list() {
            let mut os = MockOs::new();
            let queries = MockQueries::new();

            os.expect_execute_with_output_for(
                "dscl",
                vec![".", "-list", "/Users"],
                "foo\nbar\n",
                "",
                0,
            );

            let actual = MacOSCredentials {
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
