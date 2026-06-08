use crate::DockerError;
use log::Span;
use simple_rest_client::{Request, RestClient, assertions::assert_okay_with_body};

pub(crate) async fn ping(
    span: &Span,
    rest_client: &mut dyn RestClient,
) -> Result<(), DockerError> {
    let guard = span.create_child("Docker ping", log::ScopeKind::Task).start_guard();
    let req = Request::Get {
        path: "/_ping".to_string(),
        headers: vec![],
    };

    let response = rest_client.execute(guard.span(), &req).await?;
    let body = assert_okay_with_body(response)?;
    if body == "OK" {
        guard.finish_with_outcome(log::Outcome::Ok);
        Ok(())
    } else {
        guard.finish_with_outcome(log::Outcome::Failed);
        Err(DockerError::PingFailed(body))
    }
}
