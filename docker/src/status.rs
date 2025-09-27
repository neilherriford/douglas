use crate::{DockerError, SimpleDockerClient};
use serde_json::value::Value as Json;
use simple_rest_client::{Request, Response};

#[async_trait::async_trait]
pub trait Status {
    async fn ping(&mut self) -> Result<(), DockerError>;
}

#[async_trait::async_trait]
impl Status for SimpleDockerClient {
    async fn ping(&mut self) -> Result<(), DockerError> {
        let req = Request::Get {
            path: "/ping".to_string(),
            headers: vec![],
        };

        let response: Response<Vec<Json>> = self.rest_client.execute(&req).await?;
        self.expect_okay(response)?;
        Ok(())
    }
}
