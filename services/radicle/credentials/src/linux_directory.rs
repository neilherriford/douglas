use crate::directory::Directory;
use crate::os::{Os, OsError};
use std::sync::Arc;

pub struct LinuxDirectory {
    os: Arc<dyn Os>,
}

impl LinuxDirectory {
    #![allow(dead_code)]
    pub fn new(os: Arc<dyn Os + 'static>) -> Self {
        Self { os: os.clone() }
    }

    fn add_to_group(&self, user_name: &str, group_name: &str) -> Result<(), OsError> {
        if self.os.group_exists(group_name) {
            self.os.execute(
                "usermod",
                vec![
                    "--append".to_string(),
                    "--groups".to_string(),
                    group_name.to_string(),
                    user_name.to_string(),
                ],
            )
        } else {
            Err(OsError::GroupNotFoundError {
                name: group_name.to_string(),
            })
        }
    }
}

impl Directory for LinuxDirectory {
    fn create_user(
        &self,
        name: &str,
        primary_group_name: &str,
        group_names: Vec<String>,
    ) -> Result<(), OsError> {
        if !self.os.user_exists(name) {
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

        let existing = self.os.group_memberships(name);
        for group_name in group_names.iter().filter(|name| !existing.contains(name)) {
            self.add_to_group(name, group_name)?;
        }

        Ok(())
    }
    fn create_group(&self, name: &str) -> Result<(), OsError> {
        if self.os.group_exists(name) {
            return Ok(());
        }

        self.os.execute("groupadd", vec![name.to_string()])?;
        Ok(())
    }

    fn delete_user(&self, name: &str) -> Result<(), OsError> {
        if !self.os.user_exists(name) {
            return Ok(());
        }

        self.os.execute("userdel", vec![name.to_string()])?;
        Ok(())
    }

    fn delete_group(&self, name: &str) -> Result<(), OsError> {
        if !self.os.group_exists(name) {
            return Ok(());
        }

        self.os.execute("groupdel", vec![name.to_string()])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    mod create_user {
        use super::super::*;
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

            let actual = LinuxDirectory::new(Arc::new(os)).create_user(
                "foo",
                "bar",
                vec!["baz".to_string()],
            );
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_add_existing_user_to_new_group() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_group_memberships()
                .with(predicate::eq("foo"))
                .return_const(vec!["qux".to_string()]);

            os.expect_group_exists()
                .with(predicate::eq("baz"))
                .return_const(true);

            os.expect_execute()
                .withf(move |command, args| {
                    command == "usermod"
                        && *args
                            == vec![
                                "--append".to_string(),
                                "--groups".to_string(),
                                "baz".to_string(),
                                "foo".to_string(),
                            ]
                })
                .times(1)
                .return_once(move |_, _| Ok(()));

            let actual = LinuxDirectory::new(Arc::new(os)).create_user(
                "foo",
                "bar",
                vec!["baz".to_string()],
            );
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_err_if_group_does_not_exist() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_group_memberships()
                .with(predicate::eq("foo"))
                .return_const(vec!["qux".to_string()]);

            os.expect_group_exists()
                .with(predicate::eq("baz"))
                .return_const(false);

            let actual = LinuxDirectory::new(Arc::new(os)).create_user(
                "foo",
                "bar",
                vec!["baz".to_string()],
            );
            assert!(
                matches!(actual, Err(OsError::GroupNotFoundError { name }) if name == "baz".to_string() )
            );
        }

        #[test]
        fn should_add_create_user() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            os.expect_group_memberships()
                .with(predicate::eq("foo"))
                .return_const(vec![]);

            os.expect_group_exists()
                .with(predicate::eq("baz"))
                .return_const(true);

            os.expect_execute()
                .withf(move |command, args| {
                    command == "useradd"
                        && *args
                            == vec![
                                "--system".to_string(),
                                "--no-create-home".to_string(),
                                "--gid".to_string(),
                                "bar".to_string(),
                                "--home".to_string(),
                                "/nonexistent".to_string(),
                                "--shell".to_string(),
                                "/usr/sbin/nologin".to_string(),
                                "foo".to_string(),
                            ]
                })
                .times(1)
                .return_once(move |_, _| Ok(()));

            os.expect_execute()
                .withf(move |command, args| {
                    command == "usermod"
                        && *args
                            == vec![
                                "--append".to_string(),
                                "--groups".to_string(),
                                "baz".to_string(),
                                "foo".to_string(),
                            ]
                })
                .times(1)
                .return_once(move |_, _| Ok(()));

            let actual = LinuxDirectory::new(Arc::new(os)).create_user(
                "foo",
                "bar",
                vec!["baz".to_string()],
            );
            assert!(matches!(actual, Ok(())));
        }
    }

    mod create_group {
        use super::super::*;
        use crate::os::MockOs;
        use mockall::predicate;

        #[test]
        fn should_not_recreate_existing_group() {
            let mut os = MockOs::new();
            os.expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            let actual = LinuxDirectory::new(Arc::new(os)).create_group("foo");
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_create_group() {
            let mut os = MockOs::new();
            os.expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            os.expect_execute()
                .withf(move |command, args| {
                    command == "groupadd" && *args == vec!["foo".to_string()]
                })
                .times(1)
                .return_once(move |_, _| Ok(()));

            let actual = LinuxDirectory::new(Arc::new(os)).create_group("foo");
            assert!(matches!(actual, Ok(())));
        }
    }

    mod delete_user {
        use super::super::*;
        use crate::os::MockOs;
        use mockall::predicate;

        #[test]
        fn should_ignore_non_existant_user() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            let actual = LinuxDirectory::new(Arc::new(os)).delete_user("foo");
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_delete_user() {
            let mut os = MockOs::new();
            os.expect_user_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_execute()
                .withf(move |command, args| {
                    command == "userdel" && *args == vec!["foo".to_string()]
                })
                .times(1)
                .return_once(move |_, _| Ok(()));

            let actual = LinuxDirectory::new(Arc::new(os)).delete_user("foo");
            assert!(matches!(actual, Ok(())));
        }
    }

    mod delete_group {
        use super::super::*;
        use crate::os::MockOs;
        use mockall::predicate;

        #[test]
        fn should_ignore_non_existant_group() {
            let mut os = MockOs::new();
            os.expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(false);

            let actual = LinuxDirectory::new(Arc::new(os)).delete_group("foo");
            assert!(matches!(actual, Ok(())));
        }

        #[test]
        fn should_delete_group() {
            let mut os = MockOs::new();
            os.expect_group_exists()
                .with(predicate::eq("foo"))
                .return_const(true);

            os.expect_execute()
                .withf(move |command, args| {
                    command == "groupdel" && *args == vec!["foo".to_string()]
                })
                .times(1)
                .return_once(move |_, _| Ok(()));

            let actual = LinuxDirectory::new(Arc::new(os)).delete_group("foo");
            assert!(matches!(actual, Ok(())));
        }
    }
}
