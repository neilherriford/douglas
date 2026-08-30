use crate::blueprints::openbao_socket_path;
use bract_types::OpenBaoReport;
use config::DouglasFolders;
use file_system::{FileReader, FileSystemError};
use identity::Identity;
use thiserror::Error;

const ACME_PKI_ROLE: &str = "traefik";

#[derive(Error, Debug)]
pub enum OpenBaoStatusError {
    #[error("OpenBao error: {0}")]
    OpenBao(#[from] openbao::Error),
    #[error("File system error: {0}")]
    FileSystem(#[from] FileSystemError),
}

pub async fn execute(
    openbao_client_factory: &dyn openbao::ClientFactory,
    file_reader: &dyn FileReader,
    identity: &mut dyn Identity,
    is_openbao_running: bool,
    douglas_folders: &DouglasFolders,
) -> Result<OpenBaoReport, OpenBaoStatusError> {
    let mut result = OpenBaoReport::default();

    if !is_openbao_running {
        return Ok(result);
    }
    result.is_running = true;

    result.credentials_available = openbao::app_role::available(file_reader, douglas_folders);

    let socket_path = openbao_socket_path(douglas_folders);
    let mut openbao_client = openbao_client_factory.build(&socket_path).await?;

    let status = openbao_client.status().await?;
    result.is_initialized = status.initialized;
    result.is_sealed = status.sealed;

    if status.sealed || !result.credentials_available {
        return Ok(result);
    }

    let Ok(token) = openbao::app_role::login(
        openbao_client.as_mut(),
        file_reader,
        identity,
        douglas_folders,
    )
    .await
    else {
        return Ok(result);
    };
    result.credentials_work = true;

    result.mounts = openbao_client.list_mounts(&token).await.unwrap_or_default();
    result.app_role_enabled = openbao_client
        .is_auth_method_enabled(&token, &openbao_types::AuthType::AppRole)
        .await
        .unwrap_or(false);
    result.acme_enabled = openbao_client
        .is_acme_enabled(&token)
        .await
        .unwrap_or(false);
    result.root_ca_configured = openbao_client
        .root_ca_is_configured(&token)
        .await
        .unwrap_or(false);
    result.acme_pki_role_created = openbao_client
        .pki_role_exists(&token, ACME_PKI_ROLE)
        .await
        .unwrap_or(false);

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::MockFileReader;
    use identity::MockIdentity;
    use openbao::MockClientFactory;
    use std::path::PathBuf;

    fn folders() -> DouglasFolders {
        DouglasFolders {
            logs: PathBuf::from("/var/log/douglas/"),
            transients: PathBuf::from("/run/douglas/"),
            configs: PathBuf::from("/etc/douglas/"),
            seedlings_root: PathBuf::from("/var/lib/douglas/"),
            identity: PathBuf::from("/var/lib/douglas-identity/"),
        }
    }

    #[tokio::test]
    async fn execute_should_report_not_running_without_contacting_openbao() {
        let openbao_client_factory = MockClientFactory::new();
        let file_reader = MockFileReader::new();
        let mut identity = MockIdentity::new();

        let report = execute(
            &openbao_client_factory,
            &file_reader,
            &mut identity,
            false,
            &folders(),
        )
        .await
        .expect("should report not running");

        assert!(!report.is_running);
    }

    #[tokio::test]
    async fn execute_should_stop_at_sealed_without_attempting_to_log_in() {
        let mut openbao_client_factory = MockClientFactory::new();
        let mut file_reader = MockFileReader::new();
        let mut identity = MockIdentity::new();

        file_reader.expect_exists().returning(|_| true);

        let mut openbao_client = openbao::MockClient::new();
        openbao_client.expect_status().returning(|| {
            Ok(openbao_types::Status {
                initialized: true,
                sealed: true,
                ..Default::default()
            })
        });
        openbao_client_factory
            .expect_build()
            .return_once(move |_| Ok(Box::new(openbao_client)));

        identity.expect_decrypt().times(0);

        let report = execute(
            &openbao_client_factory,
            &file_reader,
            &mut identity,
            true,
            &folders(),
        )
        .await
        .expect("should report sealed");

        assert!(report.is_running);
        assert!(report.is_initialized);
        assert!(report.is_sealed);
        assert!(!report.credentials_work);
    }
}
