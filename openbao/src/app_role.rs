use crate::Client;
use config::DouglasFolders;
use file_system::{FileDeleter, FileReader, FileSystemError, FileWriter, Modes, Permissions};
use identity::Identity;
use openbao_types::AuthType;
use std::path::{Path, PathBuf};
use thiserror::Error;

const ROLE_ID_CREDENTIAL_FILE: &str = "douglas.role_id";
const SECRET_ID_CREDENTIAL_FILE: &str = "douglas.secret_id";

#[derive(Error, Debug)]
pub enum AppRoleError {
    #[error("OpenBao error: {0}")]
    OpenBao(#[from] crate::Error),
    #[error("Identity error: {0}")]
    Identity(#[from] identity::Error),
    #[error("File system error: {0}")]
    FileSystem(#[from] FileSystemError),
    #[error("Douglas's OpenBao credentials have not been provisioned yet")]
    NotAvailable,
}

fn role_id_path(douglas_folders: &DouglasFolders) -> PathBuf {
    douglas_folders.credential_file(ROLE_ID_CREDENTIAL_FILE)
}

fn secret_id_path(douglas_folders: &DouglasFolders) -> PathBuf {
    douglas_folders.credential_file(SECRET_ID_CREDENTIAL_FILE)
}

pub fn available(file_reader: &dyn FileReader, douglas_folders: &DouglasFolders) -> bool {
    file_reader.exists(&role_id_path(douglas_folders))
        && file_reader.exists(&secret_id_path(douglas_folders))
}

pub async fn login(
    openbao_client: &mut dyn Client,
    file_reader: &dyn FileReader,
    identity: &mut dyn Identity,
    douglas_folders: &DouglasFolders,
) -> Result<String, AppRoleError> {
    if !available(file_reader, douglas_folders) {
        return Err(AppRoleError::NotAvailable);
    }

    let encrypted_role_id = file_reader.read_all(&role_id_path(douglas_folders))?;
    let role_id = identity.decrypt(&identity::Intent::Authentication, encrypted_role_id)?;
    let encrypted_secret_id = file_reader.read_all(&secret_id_path(douglas_folders))?;
    let secret_id = identity.decrypt(&identity::Intent::Authentication, encrypted_secret_id)?;

    Ok(openbao_client
        .login(&AuthType::AppRole, &role_id, &secret_id)
        .await?)
}

pub fn store(
    file_writer: &dyn FileWriter,
    file_deleter: &dyn FileDeleter,
    permissions: &dyn Permissions,
    identity: &mut dyn Identity,
    douglas_folders: &DouglasFolders,
    role_id: String,
    secret_id: String,
) -> Result<(), AppRoleError> {
    write_credential_file(
        file_writer,
        file_deleter,
        permissions,
        identity,
        &role_id_path(douglas_folders),
        role_id,
    )?;
    write_credential_file(
        file_writer,
        file_deleter,
        permissions,
        identity,
        &secret_id_path(douglas_folders),
        secret_id,
    )?;
    Ok(())
}

fn write_credential_file(
    file_writer: &dyn FileWriter,
    file_deleter: &dyn FileDeleter,
    permissions: &dyn Permissions,
    identity: &mut dyn Identity,
    file_path: &Path,
    plain_text: String,
) -> Result<(), AppRoleError> {
    if file_writer.exists(file_path) {
        file_deleter.delete(file_path)?;
    }

    let encrypted = identity.encrypt(&identity::Intent::Authentication, plain_text)?;

    file_writer.write_all(file_path, &encrypted)?;
    permissions.change_user_and_group_ownership(
        file_path,
        credentials::ROOT_USER_NAME,
        credentials::ROOT_GROUP_NAME,
    )?;
    permissions.change_mode(file_path, &Modes::OwnerReadWrite)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{MockFileDeleter, MockFileReader, MockFileWriter, MockPermissions};
    use identity::MockIdentity;

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
    async fn login_should_fail_fast_when_credentials_are_missing() {
        let mut file_reader = MockFileReader::new();
        file_reader.expect_exists().returning(|_| false);
        let mut identity = MockIdentity::new();
        identity.expect_decrypt().times(0);
        let mut openbao_client = crate::MockClient::new();
        openbao_client.expect_login().times(0);

        let result = login(&mut openbao_client, &file_reader, &mut identity, &folders()).await;

        assert!(matches!(result, Err(AppRoleError::NotAvailable)));
    }

    #[tokio::test]
    async fn login_should_decrypt_both_credential_files_and_log_in_with_them() {
        let mut file_reader = MockFileReader::new();
        file_reader.expect_exists().returning(|_| true);
        file_reader
            .expect_read_all()
            .withf(|path| path.ends_with("douglas.role_id"))
            .returning(|_| Ok("encrypted-role-id".to_string()));
        file_reader
            .expect_read_all()
            .withf(|path| path.ends_with("douglas.secret_id"))
            .returning(|_| Ok("encrypted-secret-id".to_string()));

        let mut identity = MockIdentity::new();
        identity
            .expect_decrypt()
            .withf(|_, cipher_text| cipher_text == "encrypted-role-id")
            .returning(|_, _| Ok("role-id".to_string()));
        identity
            .expect_decrypt()
            .withf(|_, cipher_text| cipher_text == "encrypted-secret-id")
            .returning(|_, _| Ok("secret-id".to_string()));

        let mut openbao_client = crate::MockClient::new();
        openbao_client
            .expect_login()
            .withf(|auth_type, role_id, secret_id| {
                *auth_type == AuthType::AppRole && role_id == "role-id" && secret_id == "secret-id"
            })
            .returning(|_, _, _| Ok("session-token".to_string()));

        let token = login(&mut openbao_client, &file_reader, &mut identity, &folders())
            .await
            .expect("should log in");

        assert_eq!(token, "session-token");
    }

    #[test]
    fn store_should_delete_a_previously_written_file_first_then_encrypt_and_write() {
        let mut file_writer = MockFileWriter::new();
        file_writer.expect_exists().returning(|_| true);
        file_writer.expect_write_all().returning(|_, _| Ok(()));

        let mut file_deleter = MockFileDeleter::new();
        file_deleter.expect_delete().times(2).returning(|_| Ok(()));

        let mut permissions = MockPermissions::new();
        permissions
            .expect_change_user_and_group_ownership()
            .returning(|_, _, _| Ok(()));
        permissions.expect_change_mode().returning(|_, _| Ok(()));

        let mut identity = MockIdentity::new();
        identity
            .expect_encrypt()
            .returning(|_, plain_text| Ok(format!("encrypted:{plain_text}")));

        let result = store(
            &file_writer,
            &file_deleter,
            &permissions,
            &mut identity,
            &folders(),
            "role-1".to_string(),
            "secret-1".to_string(),
        );

        assert!(result.is_ok());
    }
}
