use super::Response;
use super::token_validator::TokenValidator;
use log::Logger;
use std::sync::Arc;
use tokio::sync::broadcast::Sender;

pub(super) struct Shutdown {
    log: Arc<dyn Logger + Sync + Send>,
    token: Arc<TokenValidator>,
}

impl Shutdown {
    pub fn new(log: Arc<dyn Logger + Sync + Send>, token_validator: Arc<TokenValidator>) -> Self {
        Self {
            log,
            token: token_validator,
        }
    }

    pub fn perform(&self, token: String, shutdown_sender: &Sender<()>) -> Response {
        self.log.info("Reporting status");
        self.token.perform_if_valid(token, move || {
            if let Err(err) = shutdown_sender.send(()) {
                let message = format!("Shutdown failed: {:?}", err);
                self.log.error(&message);
                Response::Error(message)
            } else {
                Response::Success
            }
        })
    }
}

#[cfg(test)]
mod tests {
    mod shutdown {
        use super::super::*;
        use file_system::MockFileReader;
        use log::MockLogger;
        use mockall::predicate;
        use std::path::Path;
        use tokio::sync::broadcast;

        fn build(
            token_path: &Path,
            logger: Arc<MockLogger>,
            file_reader: Arc<MockFileReader>,
        ) -> Shutdown {
            let token_validator = Arc::new(TokenValidator::new(
                logger.clone(),
                file_reader.clone(),
                token_path,
            ));

            Shutdown::new(logger.clone(), token_validator.clone())
        }

        // #[tokio::test]
        // async fn should_validate_token() {
        //     let mut log = MockLogger::new();
        //     let mut file_reader = MockFileReader::new();
        //     let token_path = Path::new("/tmp/token");

        //     log.expect_info().return_const(());
        //     file_reader
        //         .expect_read_all()
        //         .with(predicate::eq(Path::new("/tmp/token")))
        //         .returning(|_| Ok("token".to_string()));
        //     log.expect_warn()
        //         .with(predicate::eq("Invalid token"))
        //         .return_const(());

        //     let (sender, _) = broadcast::channel::<()>(1);

        //     let actual = build(token_path, Arc::new(log), Arc::new(file_reader))
        //         .perform("foo".to_string(), sender);

        //     assert!(matches!(actual, Response::InvalidToken));
        // }
    }
}
