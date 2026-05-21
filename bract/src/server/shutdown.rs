use super::Response;
use super::token_validator::TokenValidator;
use log::Span;
use std::sync::Arc;
use tokio::sync::broadcast::Sender;

pub(super) struct Shutdown {
    token: Arc<TokenValidator>,
}

impl Shutdown {
    pub fn new(token_validator: Arc<TokenValidator>) -> Self {
        Self {
            token: token_validator,
        }
    }

    pub fn perform(&self, span: &Span, token: String, shutdown_sender: &Sender<()>) -> Response {
        self.token.perform_if_valid(&span, token, move || {
            let log = span.create_scoped_reporter();
            if let Err(err) = shutdown_sender.send(()) {
                let message = format!("Shutdown failed: {:?}", err);
                log.message(log::Level::Warn, &message);
                log.finish(log::Outcome::Failed);
                Response::Error(message)
            } else {
                log.finish(log::Outcome::Ok);
                Response::Success
            }
        })
    }
}
