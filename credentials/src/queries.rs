#[cfg(test)]
use mockall::automock;
use users::{get_group_by_name, get_user_by_name};

#[cfg_attr(test, automock)]
pub(crate) trait Queries: Send + Sync {
    fn group_memberships(&self, name: &str) -> Vec<String>;
    fn get_group_id(&self, name: &str) -> Option<u32>;
    fn get_user_id(&self, name: &str) -> Option<u32>;
    fn is_root(&self) -> bool;
    fn user_exists(&self, name: &str) -> bool;
    fn group_exists(&self, name: &str) -> bool;
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
        get_user_by_name(name)
            .and_then(|user| user.groups())
            .map(|groups| {
                groups
                    .iter()
                    .filter_map(|g| g.name().to_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn get_group_id(&self, name: &str) -> Option<u32> {
        get_group_by_name(name).map(|group| group.gid())
    }

    fn get_user_id(&self, name: &str) -> Option<u32> {
        get_user_by_name(name).map(|user| user.uid())
    }

    fn is_root(&self) -> bool {
        nix::unistd::Uid::effective().is_root()
    }

    fn user_exists(&self, name: &str) -> bool {
        get_user_by_name(name).is_some()
    }

    fn group_exists(&self, name: &str) -> bool {
        get_group_by_name(name).is_some()
    }
}
