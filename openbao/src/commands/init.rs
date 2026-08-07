use crate::{OpenBaoError, Secret, Secrets};
use log::{Reporter, Span};
use serde::{Deserialize, Serialize};
use serde_json::from_value;
use simple_rest_client::{
    Header, Request, RestClient,
    assertions::assert_okay_with_body,
    parsers::{Parser, json::JsonParser},
};
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("The threshold must be less than or equal to the number of shares")]
    InvalidThreshold,

    #[error("The threshold must non-zero")]
    ThresholdTooSmall,

    #[error("The shares must non-zero")]
    SharesTooSmall,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Config {
    pub secret_shares: u8,
    pub secret_threshold: u8,
}

#[derive(Debug, PartialEq, Deserialize)]
struct Status {
    initialized: bool,
}

impl Config {
    pub fn new(secret_shares: u8, secret_threshold: u8) -> Result<Self, ConfigError> {
        if secret_shares == 0 {
            Err(ConfigError::SharesTooSmall)
        } else if secret_threshold == 0 {
            Err(ConfigError::ThresholdTooSmall)
        } else if secret_threshold > secret_shares {
            Err(ConfigError::InvalidThreshold)
        } else {
            Ok(Self {
                secret_shares,
                secret_threshold,
            })
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
struct Response {
    keys: Vec<String>,
    keys_base64: Vec<String>,
    root_token: String,
}

pub struct InitCommand<'a> {
    reporter: Arc<dyn Reporter>,
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    config: &'a Config,
}

impl<'a> InitCommand<'a> {
    pub fn new(
        reporter: Arc<dyn Reporter>,
        rest_client: &'a mut dyn RestClient,
        parser: &'a JsonParser,
        config: &'a Config,
    ) -> Self {
        Self {
            reporter,
            rest_client,
            parser,
            config,
        }
    }

    pub async fn perform(&mut self) -> Result<Secrets, OpenBaoError> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "OpenBao initialize",
            log::ScopeKind::Task,
        )
        .start_guard();

        if self.is_initialized(guard.span()).await? {
            return Err(OpenBaoError::AlreadyInitialized);
        }
        guard.finish(self.initialize(guard.span()).await)
    }

    async fn is_initialized(&mut self, span: &Span) -> Result<bool, OpenBaoError> {
        let req = Request::Get {
            path: "/v1/sys/init".to_string(),
            headers: vec![],
        };

        let body = assert_okay_with_body(self.rest_client.execute(span, &req).await?)?;

        match self.parser.parse(body) {
            Ok(json) => {
                let status: Status = from_value(json)?;
                Ok(status.initialized)
            }
            Err(err) => Err(OpenBaoError::ParseError {
                line: 0,
                column: 0,
                message: format!("{err}"),
            }),
        }
    }

    async fn initialize(&mut self, span: &Span) -> Result<Secrets, OpenBaoError> {
        let req = Request::Post {
            path: "/v1/sys/init".to_string(),
            headers: vec![Header::content_type_json()],
            body: Some(serde_json::to_string(&self.config)?),
        };

        let body = assert_okay_with_body(self.rest_client.execute(span, &req).await?)?;

        match self.parser.parse(body) {
            Ok(json) => {
                let response: Response = from_value(json)?;
                let secrets = (0..response.keys.len())
                    .map(|index| Secret {
                        key: response.keys[index].clone(),
                        base64: response.keys_base64[index].clone(),
                    })
                    .collect();

                Ok(Secrets {
                    secrets,
                    root_token: response.root_token,
                })
            }
            Err(err) => Err(OpenBaoError::ParseError {
                line: 0,
                column: 0,
                message: format!("{err}"),
            }),
        }
    }
}
