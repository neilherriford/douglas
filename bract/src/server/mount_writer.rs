use super::{Response, mount_io::MountIo, token_validator::TokenValidator};
use log::Logger;
use std::{path::PathBuf, sync::Arc};
use utils::ClientErrorDisplay;

pub struct MountWriter {
    log: Arc<dyn Logger + Sync + Send>,
    token: Arc<TokenValidator>,
    mount_io: Arc<MountIo>,
}

impl MountWriter {
    pub fn new(
        log: Arc<dyn Logger + Sync + Send>,
        token_validator: Arc<TokenValidator>,
        mount_io: Arc<MountIo>,
    ) -> Self {
        Self {
            log,
            token: token_validator,
            mount_io,
        }
    }

    pub fn perform(
        &self,
        token: String,
        service_name: &str,
        mount_name: &str,
        relative_path: PathBuf,
        contents: &str,
    ) -> Response {
        self.log.info(&format!(
            "Writing into active mount for {service_name} {mount_name} {relative_path:?}",
        ));
        todo!();

        // self.token.perform_if_valid(token, move || {
        //     or_log_and_return_response_error!(
        //         self.log => warn,
        //         self.mount_io
        //             .write_file(service_name, mount_name, relative_path, contents)
        //     );

        //     Response::WroteToMount
        // })
    }
}
