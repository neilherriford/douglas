#[cfg(test)]
use mockall::automock;
use std::sync::Mutex;
use users::{get_group_by_name, get_user_by_name, os::unix::GroupExt};

// getpwnam/getgrnam is not thread safe
static USERS_LOCK: Mutex<()> = Mutex::new(());

#[cfg_attr(test, automock)]
pub(crate) trait Queries: Send + Sync {
    fn group_memberships(&self, name: &str) -> Vec<String>;
    fn get_group_id(&self, name: &str) -> Option<u32>;
    fn get_user_id(&self, name: &str) -> Option<u32>;
    fn is_root(&self) -> bool;
    fn user_exists(&self, name: &str) -> bool;
    fn group_exists(&self, name: &str) -> bool;
    fn get_primary_group_id(&self, name: &str) -> Option<u32>;
}

#[derive(Default)]
pub(crate) struct LocalQueries {}

impl LocalQueries {
    pub fn new() -> Self {
        Self {}
    }
}

impl Queries for LocalQueries {
    fn group_memberships(&self, name: &str) -> Vec<String> {
        let _guard = USERS_LOCK.lock().unwrap();
        get_group_by_name(name)
            .map(|group| {
                group
                    .members()
                    .iter()
                    .filter_map(|m| m.to_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn get_group_id(&self, name: &str) -> Option<u32> {
        let _guard = USERS_LOCK.lock().unwrap();
        get_group_by_name(name).map(|group| group.gid())
    }

    fn get_user_id(&self, name: &str) -> Option<u32> {
        let _guard = USERS_LOCK.lock().unwrap();
        get_user_by_name(name).map(|user| user.uid())
    }

    fn is_root(&self) -> bool {
        nix::unistd::Uid::effective().is_root()
    }

    fn user_exists(&self, name: &str) -> bool {
        let _guard = USERS_LOCK.lock().unwrap();
        get_user_by_name(name).is_some()
    }

    fn group_exists(&self, name: &str) -> bool {
        let _guard = USERS_LOCK.lock().unwrap();
        get_group_by_name(name).is_some()
    }

    fn get_primary_group_id(&self, name: &str) -> Option<u32> {
        let _guard = USERS_LOCK.lock().unwrap();
        get_user_by_name(name).map(|user| user.primary_group_id())
    }
}
