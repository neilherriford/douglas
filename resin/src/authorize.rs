use crate::ServerError;
use resin_types::{Name, Repository};

pub(crate) async fn authorize_write(
    repository: &Repository,
    seedling_registration_client: &dyn seedling_registration_client::Client,
) -> Result<Name, ServerError> {
    let name = repository.require_local()?.clone();

    match seedling_registration_client
        .seedling_registered(&name.to_string())
        .await?
    {
        seedling_registration_types::Response::Registered => Ok(name),
        seedling_registration_types::Response::NotRegistered => {
            Err(ServerError::SeedlingNotRegistered(name.to_string()))
        }
        seedling_registration_types::Response::InvalidName => Err(ServerError::InvalidName(
            format!("invalid seedling name '{name}'"),
        )),
        seedling_registration_types::Response::Reserved => {
            Err(ServerError::SeedlingReserved(name.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::authorize_write;
    use crate::ServerError;
    use resin_types::Repository;
    use seedling_registration_client::MockClient;
    use seedling_registration_types::Response;

    fn local(name: &str) -> Repository {
        name.parse().unwrap()
    }

    fn upstream() -> Repository {
        "ghcr.io/foo/bar".parse().unwrap()
    }

    #[tokio::test]
    async fn test_should_succeed_when_the_seedling_is_registered() {
        let mut client = MockClient::new();
        client
            .expect_seedling_registered()
            .returning(|_| Ok(Response::Registered));

        let result = authorize_write(&local("traefik"), &client).await;

        assert!(matches!(result, Ok(name) if name.to_string() == "traefik"));
    }

    #[tokio::test]
    async fn test_should_reject_when_the_seedling_is_not_registered() {
        let mut client = MockClient::new();
        client
            .expect_seedling_registered()
            .returning(|_| Ok(Response::NotRegistered));

        let result = authorize_write(&local("traefik"), &client).await;

        assert!(matches!(result, Err(ServerError::SeedlingNotRegistered(_))));
    }

    #[tokio::test]
    async fn test_should_reject_when_the_name_is_invalid() {
        let mut client = MockClient::new();
        client
            .expect_seedling_registered()
            .returning(|_| Ok(Response::InvalidName));

        let result = authorize_write(&local("traefik"), &client).await;

        assert!(matches!(result, Err(ServerError::InvalidName(_))));
    }

    #[tokio::test]
    async fn test_should_bubble_up_transport_errors() {
        let mut client = MockClient::new();
        client
            .expect_seedling_registered()
            .returning(|_| Err(seedling_registration_client::Error::ConnectionRefused));

        let result = authorize_write(&local("traefik"), &client).await;

        assert!(matches!(result, Err(ServerError::Internal(_))));
    }

    #[tokio::test]
    async fn test_should_reject_a_reserved_seedling() {
        let mut client = MockClient::new();
        client
            .expect_seedling_registered()
            .returning(|_| Ok(Response::Reserved));

        let result = authorize_write(&local("traefik"), &client).await;

        assert!(matches!(result, Err(ServerError::SeedlingReserved(_))));
    }

    #[tokio::test]
    async fn test_should_reject_an_upstream_repository_before_checking_registration() {
        let mut client = MockClient::new();
        client.expect_seedling_registered().times(0);

        let result = authorize_write(&upstream(), &client).await;

        assert!(matches!(result, Err(ServerError::MethodNotAllowed(_))));
    }
}
