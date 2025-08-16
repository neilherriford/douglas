#[cfg(test)]
use mockall::automock;
use users::{get_group_by_name, get_user_by_name};

#[cfg_attr(test, automock)]
pub(crate) trait Queries {
    fn group_memberships(&self, name: &str) -> Vec<String>;
    fn get_group_id(&self, name: &str) -> Option<u32>;
    fn is_root(&self) -> bool;
    fn user_exists(&self, name: &str) -> bool;
    fn group_exists(&self, name: &str) -> bool;
}

pub(crate) struct LocalQueries {}

impl LocalQueries {
    pub fn new() -> Self {
        Self {}
    }
}

impl Queries for LocalQueries {
    fn group_memberships(&self, name: &str) -> Vec<String> {
        if let Some(user) = get_user_by_name(name) {
            if let Some(groups) = user.groups() {
                return groups
                    .iter()
                    .filter_map(|group| group.name().to_str())
                    .map(|group_name| group_name.to_string())
                    .collect();
            }
        }

        vec![]
    }

    fn get_group_id(&self, name: &str) -> Option<u32> {
        if let Some(group) = get_group_by_name(name) {
            Some(group.gid())
        } else {
            None
        }
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
