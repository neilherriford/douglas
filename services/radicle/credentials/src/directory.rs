use crate::os::OsError;
use mockall::automock;

#[automock]
pub trait Directory {
    fn create_user(
        &self,
        name: &str,
        primary_group_name: &str,
        group_names: Vec<String>,
    ) -> Result<(), OsError>;
    fn create_group(&self, name: &str) -> Result<(), OsError>;
    fn delete_user(&self, name: &str) -> Result<(), OsError>;
    fn delete_group(&self, name: &str) -> Result<(), OsError>;
}
