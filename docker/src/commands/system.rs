use crate::{DockerError, to_general_error};
use log::{Reporter, Span};
use simple_rest_client::{Request, RestClient, assertions::assert_okay_with_body};
use std::sync::Arc;

pub(crate) async fn ping(
    reporter: Arc<dyn Reporter>,
    rest_client: &tokio::sync::Mutex<dyn RestClient + Send + Sync + 'static>,
) -> Result<(), DockerError> {
    let guard = Span::new(reporter, "Docker ping", log::ScopeKind::Task).start_guard();
    let req = Request::Get {
        path: "/_ping".to_string(),
        headers: vec![],
    };
    let mut rest_client = rest_client.lock().await;
    let response = rest_client
        .execute(guard.span(), &req)
        .await
        .map_err(to_general_error)?;
    let body = assert_okay_with_body(response)?;
    if body == "OK" {
        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    } else {
        guard.finish_with_outcome(log::Outcome::Failed);
        Err(DockerError::PingFailed(body))
    }
}
