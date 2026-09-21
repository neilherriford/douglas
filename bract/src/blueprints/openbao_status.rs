use crate::blueprints::openbao_socket_path;
use bract_types::{DouglasCredentialsReport, InstalledReport, OpenBaoReport};
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
    if !is_openbao_running {
        return Ok(OpenBaoReport::NotRunning);
    }

    let credentials_available = openbao::app_role::available(file_reader, douglas_folders);

    let socket_path = openbao_socket_path(douglas_folders);
    let mut openbao_client = openbao_client_factory.build(&socket_path).await?;

    match openbao_client.status().await?.seal_state() {
        openbao_types::SealState::Uninitialized => {
            return Ok(OpenBaoReport::Uninitialized {
                credentials_available,
            });
        }
        openbao_types::SealState::Sealed => {
            return Ok(OpenBaoReport::Sealed {
                credentials_available,
            });
        }
        openbao_types::SealState::Unsealed => {}
    }

    if !credentials_available {
        return Ok(unsealed(DouglasCredentialsReport::Unavailable));
    }

    let Ok(token) = openbao::app_role::login(
        openbao_client.as_mut(),
        file_reader,
        identity,
        douglas_folders,
    )
    .await
    else {
        return Ok(unsealed(DouglasCredentialsReport::NotWorking));
    };

    Ok(unsealed(DouglasCredentialsReport::Working(
        installed(openbao_client.as_mut(), &token).await,
    )))
}

fn unsealed(credentials: DouglasCredentialsReport) -> OpenBaoReport {
    OpenBaoReport::Unsealed { credentials }
}

async fn installed(openbao_client: &mut dyn openbao::Client, token: &str) -> InstalledReport {
    InstalledReport {
        mounts: openbao_client.list_mounts(token).await.unwrap_or_default(),
        app_role_enabled: openbao_client
            .is_auth_method_enabled(token, &openbao_types::AuthType::AppRole)
            .await
            .unwrap_or(false),
        acme_enabled: openbao_client.is_acme_enabled(token).await.unwrap_or(false),
        root_ca_configured: openbao_client
            .root_ca_is_configured(token)
            .await
            .unwrap_or(false),
        acme_pki_role_created: openbao_client
            .pki_role_exists(token, ACME_PKI_ROLE)
            .await
            .unwrap_or(false),
    }
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

        assert_eq!(report, OpenBaoReport::NotRunning);
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

        assert_eq!(
            report,
            OpenBaoReport::Sealed {
                credentials_available: true
            }
        );
    }

    fn factory_with_status(
        initialized: bool,
        sealed: bool,
        configure: impl FnOnce(&mut openbao::MockClient),
    ) -> MockClientFactory {
        let mut openbao_client = openbao::MockClient::new();
        openbao_client.expect_status().returning(move || {
            Ok(openbao_types::Status {
                initialized,
                sealed,
                ..Default::default()
            })
        });
        configure(&mut openbao_client);
        let mut factory = MockClientFactory::new();
        factory
            .expect_build()
            .return_once(move |_| Ok(Box::new(openbao_client)));
        factory
    }

    fn file_reader(credentials_available: bool) -> MockFileReader {
        let mut file_reader = MockFileReader::new();
        file_reader
            .expect_exists()
            .returning(move |_| credentials_available);
        file_reader
            .expect_read_all()
            .returning(|_| Ok("encrypted".to_string()));
        file_reader
    }

    #[tokio::test]
    async fn execute_should_report_uninitialized_with_whether_douglas_credentials_exist() {
        let factory = factory_with_status(false, true, |_| {});
        let file_reader = file_reader(true);
        let mut identity = MockIdentity::new();

        let report = execute(&factory, &file_reader, &mut identity, true, &folders())
            .await
            .expect("should report");

        assert_eq!(
            report,
            OpenBaoReport::Uninitialized {
                credentials_available: true
            }
        );
    }

    #[tokio::test]
    async fn execute_should_report_unavailable_credentials_when_unsealed_without_them() {
        let factory = factory_with_status(true, false, |_| {});
        let file_reader = file_reader(false);
        let mut identity = MockIdentity::new();
        identity.expect_decrypt().times(0);

        let report = execute(&factory, &file_reader, &mut identity, true, &folders())
            .await
            .expect("should report");

        assert_eq!(
            report,
            OpenBaoReport::Unsealed {
                credentials: DouglasCredentialsReport::Unavailable
            }
        );
    }

    #[tokio::test]
    async fn execute_should_report_credentials_that_do_not_work_when_the_login_fails() {
        let factory = factory_with_status(true, false, |client| {
            client
                .expect_login()
                .returning(|_, _, _| Err(openbao::Error::NotAuthenticated));
        });
        let file_reader = file_reader(true);
        let mut identity = MockIdentity::new();
        identity
            .expect_decrypt()
            .returning(|_, _| Ok("decrypted".to_string()));

        let report = execute(&factory, &file_reader, &mut identity, true, &folders())
            .await
            .expect("should report");

        assert_eq!(
            report,
            OpenBaoReport::Unsealed {
                credentials: DouglasCredentialsReport::NotWorking
            }
        );
    }

    #[tokio::test]
    async fn execute_should_report_what_is_installed_when_the_login_works() {
        let factory = factory_with_status(true, false, |client| {
            client
                .expect_login()
                .returning(|_, _, _| Ok("token".to_string()));
            client.expect_list_mounts().returning(|_| {
                Ok(std::collections::HashMap::from([(
                    "kv/".to_string(),
                    "kv".to_string(),
                )]))
            });
            client
                .expect_is_auth_method_enabled()
                .returning(|_, _| Ok(true));
            client.expect_is_acme_enabled().returning(|_| Ok(false));
            client
                .expect_root_ca_is_configured()
                .returning(|_| Ok(true));
            client.expect_pki_role_exists().returning(|_, _| Ok(false));
        });
        let file_reader = file_reader(true);
        let mut identity = MockIdentity::new();
        identity
            .expect_decrypt()
            .returning(|_, _| Ok("decrypted".to_string()));

        let report = execute(&factory, &file_reader, &mut identity, true, &folders())
            .await
            .expect("should report");

        assert_eq!(
            report,
            OpenBaoReport::Unsealed {
                credentials: DouglasCredentialsReport::Working(InstalledReport {
                    mounts: std::collections::HashMap::from([(
                        "kv/".to_string(),
                        "kv".to_string()
                    )]),
                    app_role_enabled: true,
                    acme_enabled: false,
                    root_ca_configured: true,
                    acme_pki_role_created: false,
                })
            }
        );
    }

    #[tokio::test]
    async fn execute_should_treat_a_query_that_errors_as_not_installed() {
        let factory = factory_with_status(true, false, |client| {
            client
                .expect_login()
                .returning(|_, _, _| Ok("token".to_string()));
            client
                .expect_list_mounts()
                .returning(|_| Err(openbao::Error::NotAuthenticated));
            client
                .expect_is_auth_method_enabled()
                .returning(|_, _| Err(openbao::Error::NotAuthenticated));
            client
                .expect_is_acme_enabled()
                .returning(|_| Err(openbao::Error::NotAuthenticated));
            client
                .expect_root_ca_is_configured()
                .returning(|_| Err(openbao::Error::NotAuthenticated));
            client
                .expect_pki_role_exists()
                .returning(|_, _| Err(openbao::Error::NotAuthenticated));
        });
        let file_reader = file_reader(true);
        let mut identity = MockIdentity::new();
        identity
            .expect_decrypt()
            .returning(|_, _| Ok("decrypted".to_string()));

        let report = execute(&factory, &file_reader, &mut identity, true, &folders())
            .await
            .expect("should report");

        assert_eq!(
            report,
            OpenBaoReport::Unsealed {
                credentials: DouglasCredentialsReport::Working(InstalledReport::default())
            }
        );
    }
}
