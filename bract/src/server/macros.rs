macro_rules! or_log_and_return_response_error {
    ($logger:expr => $method:ident, $result:expr) => {{
        match $result {
            Ok(val) => val,
            Err(err) => {
                $logger.$method(&err.to_string());
                return Response::Error(err.to_client_string());
            }
        }
    }};
}

#[cfg(test)]
mod tests {
    mod or_log_and_return_error {
        use super::super::super::Response;
        use log::{Logger, MockLogger};
        use mockall::mock;

        trait FakeError {
            fn to_client_string(&self) -> String;
            fn to_string(&self) -> String;
        }

        mock! {
            pub Error {}

            impl FakeError for Error {
                fn to_client_string(&self) -> String;
                fn to_string(&self) -> String;
            }
        }

        #[test]
        fn should_log_and_return_error() {
            let mut logger = MockLogger::new();
            let mut error = MockError::new();

            logger
                .expect_warn()
                .withf(|msg| msg == "server-facing message")
                .return_const(());

            error
                .expect_to_client_string()
                .return_const("client-facing message".to_string());

            error
                .expect_to_string()
                .return_const("server-facing message".to_string());

            fn test(logger: impl Logger, result: Result<String, MockError>) -> Response {
                let _ = or_log_and_return_response_error!(logger => warn, result);
                Response::InvalidToken
            }

            let actual = test(logger, Err(error));

            assert!(matches!(actual, Response::Error(msg) if msg == "client-facing message"));
        }

        #[test]
        fn should_return_succesful_result() {
            let logger = MockLogger::new();

            fn test(logger: impl Logger, result: Result<String, MockError>) -> Response {
                let success = or_log_and_return_response_error!(logger => warn, result);
                Response::CredentialsCreated {
                    user: success,
                    user_id: 123,
                    group: "bar".into(),
                    group_id: 321,
                }
            }

            let actual = test(logger, Ok("foo".to_string()));

            assert!(matches!(actual, Response::CredentialsCreated {user, .. } if user == "foo"));
        }
    }
}
