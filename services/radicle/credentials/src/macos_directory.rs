use crate::directory::Directory;
use crate::os::{Os, OsError};
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

pub struct MacOSDirectory {
    os: Arc<dyn Os + 'static>,
}

impl MacOSDirectory {
    pub fn new(os: Arc<dyn Os + 'static>) -> Self {
        Self { os: os.clone() }
    }

    fn execute(
        &self,
        operation: Operation,
        path: String,
        args: Vec<String>,
    ) -> Result<Output, OsError> {
        let mut arguments = vec![".".to_string(), operation.to_string(), path];
        for arg in &args {
            arguments.push(arg.to_string())
        }
        self.os.execute_with_output("dscl", arguments)
    }

    fn named_object_path(&self, name: &str, kind: ObjectKind) -> String {
        format!("/{}/{}", kind, name)
    }

    fn list(&self, kind: ObjectKind, attribute: ObjectAttribute) -> Result<Output, OsError> {
        let path = format!("/{}", kind);
        self.execute(Operation::List, path, vec![attribute.to_string()])
    }

    fn create_object(&self, name: &str, kind: ObjectKind) -> Result<(), OsError> {
        let path = self.named_object_path(name, kind);
        self.execute(Operation::Create, path, vec![])?;
        Ok(())
    }

    fn delete_object(&self, name: &str, kind: ObjectKind) -> Result<(), OsError> {
        let path = self.named_object_path(name, kind);
        self.execute(Operation::Delete, path, vec![])?;
        Ok(())
    }

    fn set_object_attribute(
        &self,
        name: &str,
        kind: ObjectKind,
        attribute: ObjectAttribute,
        value: String,
    ) -> Result<(), OsError> {
        let path = self.named_object_path(name, kind);
        self.execute(Operation::Create, path, vec![attribute.to_string(), value])?;
        Ok(())
    }

    fn get_first_unused_id(&self, kind: ObjectKind, min_id: u32) -> Result<u32, OsError> {
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

    fn add_to_group(&self, user_name: &str, group_name: String) -> Result<(), OsError> {
        if self.os.group_exists(&group_name) {
            self.set_object_attribute(
                &group_name,
                ObjectKind::Group,
                ObjectAttribute::GroupMembership,
                user_name.to_string(),
            )
        } else {
            Err(OsError::GroupNotFoundError { name: group_name })
        }
    }
}

static MINIMUM_ID: u32 = 501;

impl Directory for MacOSDirectory {
    fn create_user(
        &self,
        name: &str,
        primary_group_name: &str,
        group_names: Vec<String>,
    ) -> Result<(), OsError> {
        if !self.os.user_exists(name) {
            let uid = self.get_first_unused_id(ObjectKind::User, MINIMUM_ID)?;

            let primary_gid =
                self.os
                    .get_group_id(primary_group_name)
                    .ok_or(OsError::GroupNotFoundError {
                        name: primary_group_name.to_string(),
                    })?;

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

        let existing = self.os.group_memberships(name);
        for group_name in group_names.iter().filter(|name| !existing.contains(name)) {
            self.add_to_group(name, group_name.to_string())?;
        }

        Ok(())
    }

    fn create_group(&self, name: &str) -> Result<(), OsError> {
        if self.os.group_exists(name) {
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
    fn delete_user(&self, name: &str) -> Result<(), OsError> {
        if !self.os.user_exists(name) {
            return Ok(());
        }

        self.delete_object(name, ObjectKind::User)?;
        Ok(())
    }
    fn delete_group(&self, name: &str) -> Result<(), OsError> {
        if !self.os.group_exists(name) {
            return Ok(());
        }
        self.delete_object(name, ObjectKind::Group)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    static EMPTY_STDERR: &str = "";
    static STATUS_SUCCESS: i32 = 0;

    mod create_user {
        use super::super::*;
        use super::*;
        use crate::os::MockOs;
        use mockall::predicate;

        #[test]
        fn should_not_recreate_existing_user() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_group_memberships()
                .with(predicate::eq("foo"))
                .return_const(vec!["baz".to_string()]);

            let actual = MacOSDirectory::new(Arc::new(os)).create_user(
                "foo",
                "bar",
                vec!["baz".to_string()],
            );
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_err_if_new_group_does_not_exist() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_group_memberships()
                .with(predicate::eq("foo"))
                .return_const(vec!["baz".to_string()]);

            os.expect_group_exists()
                .with(predicate::eq("qux"))
                .return_const(false);

            let actual = MacOSDirectory::new(Arc::new(os)).create_user(
                "foo",
                "bar",
                vec!["qux".to_string()],
            );
            assert!(
                matches!(actual, Err(OsError::GroupNotFoundError { name }) if name == "qux".to_string() )
            );
        }

        #[test]
        fn should_add_existing_user_to_new_group() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_group_memberships()
                .with(predicate::eq("foo"))
                .return_const(vec!["baz".to_string()]);

            os.expect_group_exists()
                .with(predicate::eq("qux"))
                .return_const(true);

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".".to_string(),
                    "-create".to_string(),
                    "/Groups/qux".to_string(),
                    "GroupMembership".to_string(),
                    "foo".to_string(),
                ],
            );

            let actual = MacOSDirectory::new(Arc::new(os)).create_user(
                "foo",
                "bar",
                vec!["qux".to_string()],
            );
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_error_if_primary_group_not_found() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".".to_string(),
                    "-list".to_string(),
                    "/Users".to_string(),
                    "UniqueID".to_string(),
                ],
            );

            os.expect_get_group_id()
                .with(predicate::eq("bar"))
                .return_const(None);

            let actual = MacOSDirectory::new(Arc::new(os)).create_user("foo", "bar", vec![]);
            assert!(
                matches!(actual, Err(OsError::GroupNotFoundError { name }) if name == "bar".to_string() )
            );
        }

        #[test]
        fn should_create_user() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            os.expect_execute_with_output_for(
                "dscl",
                vec![
                    ".".to_string(),
                    "-list".to_string(),
                    "/Users".to_string(),
                    "UniqueID".to_string(),
                ],
                r#"
bar        501
baz        502
qux        504
                "#,
                EMPTY_STDERR,
                STATUS_SUCCESS,
            );

            os.expect_get_group_id()
                .with(predicate::eq("bar"))
                .return_const(Some(1234));

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".".to_string(),
                    "-create".to_string(),
                    "/Users/foo".to_string(),
                ],
            );
            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".".to_string(),
                    "-create".to_string(),
                    "/Users/foo".to_string(),
                    "UniqueID".to_string(),
                    "503".to_string(),
                ],
            );

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".".to_string(),
                    "-create".to_string(),
                    "/Users/foo".to_string(),
                    "UserShell".to_string(),
                    "/usr/bin/false".to_string(),
                ],
            );

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".".to_string(),
                    "-create".to_string(),
                    "/Users/foo".to_string(),
                    "NFSHomeDirectory".to_string(),
                    "/var/empty".to_string(),
                ],
            );

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".".to_string(),
                    "-create".to_string(),
                    "/Users/foo".to_string(),
                    "PrimaryGroupID".to_string(),
                    "1234".to_string(),
                ],
            );

            os.expect_group_memberships()
                .with(predicate::eq("foo"))
                .return_const(vec![]);

            let actual = MacOSDirectory::new(Arc::new(os)).create_user("foo", "bar", vec![]);

            assert!(matches!(actual, Ok(())));
        }
    }

    mod create_group {
        use super::super::*;
        use super::*;
        use crate::os::MockOs;
        use mockall::predicate;

        #[test]
        fn should_not_create_existing_group() {
            let mut os = MockOs::new();
            os.expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            let actual = MacOSDirectory::new(Arc::new(os)).create_group("foo");
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_create_group() {
            let mut os = MockOs::new();
            os.expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            os.expect_execute_with_output_for(
                "dscl",
                vec![
                    ".".to_string(),
                    "-list".to_string(),
                    "/Groups".to_string(),
                    "PrimaryGroupID".to_string(),
                ],
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
                vec![
                    ".".to_string(),
                    "-create".to_string(),
                    "/Groups/foo".to_string(),
                ],
            );

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".".to_string(),
                    "-create".to_string(),
                    "/Groups/foo".to_string(),
                    "PrimaryGroupID".to_string(),
                    "503".to_string(),
                ],
            );

            let actual = MacOSDirectory::new(Arc::new(os)).create_group("foo");
            assert!(matches!(actual, Ok(())));
        }
    }

    mod delete_user {
        use super::super::*;
        use crate::os::MockOs;
        use mockall::predicate;

        #[test]
        fn should_ignore_non_exsiting_users() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            let actual = MacOSDirectory::new(Arc::new(os)).delete_user("foo");
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_delete_user() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".".to_string(),
                    "-delete".to_string(),
                    "/Users/foo".to_string(),
                ],
            );

            let actual = MacOSDirectory::new(Arc::new(os)).delete_user("foo");
            assert!(matches!(actual, Ok(())));
        }
    }

    mod delete_group {
        use super::super::*;
        use crate::os::MockOs;
        use mockall::predicate;

        #[test]
        fn should_ignore_non_exsiting_groups() {
            let mut os = MockOs::new();
            os.expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            let actual = MacOSDirectory::new(Arc::new(os)).delete_group("foo");
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_delete_group() {
            let mut os = MockOs::new();
            os.expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_execute_with_output_with_empty_success(
                "dscl",
                vec![
                    ".".to_string(),
                    "-delete".to_string(),
                    "/Groups/foo".to_string(),
                ],
            );

            let actual = MacOSDirectory::new(Arc::new(os)).delete_group("foo");
            assert!(matches!(actual, Ok(())));
        }
    }
}
