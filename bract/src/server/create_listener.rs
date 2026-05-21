use super::ServerError;
use config::constants::DOUGLAS_ADMIN_GROUP;
use file_system::{FileDeleter, Listener, Modes, Permissions, UnixDomainSocket};
use log::{Outcome, ScopeKind, Span};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) struct CreateListener {
    socket_path: PathBuf,
    file_deleter: Arc<dyn FileDeleter>,
    permissions: Arc<dyn Permissions>,
    unix_domain_socket: Arc<dyn UnixDomainSocket>,
}

impl CreateListener {
    pub fn new(
        socket_path: &Path,
        file_deleter: Arc<dyn FileDeleter>,
        permissions: Arc<dyn Permissions>,
        unix_domain_socket: Arc<dyn UnixDomainSocket>,
    ) -> Self {
        Self {
            socket_path: socket_path.to_path_buf(),
            file_deleter,
            permissions,
            unix_domain_socket,
        }
    }

    pub fn create(
        &self,
        span: &Span,
    ) -> Result<Box<dyn Listener + Send + Sync + 'static>, ServerError> {
        let child_span = span.create_child(
            &format!("Refreshing socket '{}'", self.socket_path.to_string_lossy()),
            ScopeKind::Step,
        );
        let log = child_span.create_scoped_reporter();
        self.file_deleter.delete(&self.socket_path)?;

        let listener = self.unix_domain_socket.bind(&self.socket_path)?;

        self.permissions.change_user_and_group_ownership(
            &self.socket_path,
            credentials::ROOT_USER_NAME,
            DOUGLAS_ADMIN_GROUP,
        )?;
        self.permissions
            .change_mode(&self.socket_path, &Modes::OwnerReadWriteGroupReadWrite)?;

        log.finish(Outcome::Ok);
        Ok(listener)
    }
}
