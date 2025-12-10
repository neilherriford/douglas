use crate::{OpenBaoError, Secret, Secrets};
use serde::{Deserialize, Serialize};
use serde_json::from_value;
use simple_rest_client::{
    Request, RestClient,
    assertions::assert_okay_with_body,
    parsers::{Parser, json::JsonParser},
};
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
struct Resposne {
    keys: Vec<String>,
    keys_base64: Vec<String>,
    root_token: String,
}

pub struct InitCommand<'a> {
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    config: Config,
}

impl<'a> InitCommand<'a> {
    pub fn new(
        rest_client: &'a mut dyn RestClient,
        parser: &'a JsonParser,
        config: Config,
    ) -> Self {
        Self {
            rest_client,
            parser,
            config,
        }
    }

    pub async fn perform(&mut self) -> Result<Secrets, OpenBaoError> {
        if self.is_intialized().await? {
            return Err(OpenBaoError::AlreadyInitialized);
        }
        self.intialize().await
    }

    async fn is_intialized(&mut self) -> Result<bool, OpenBaoError> {
        let req = Request::Get {
            path: "/v1/sys/init".to_string(),
            headers: vec![],
        };

        let body = assert_okay_with_body(self.rest_client.execute(&req).await?)?;

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

    async fn intialize(&mut self) -> Result<Secrets, OpenBaoError> {
        let req = Request::Post {
            path: "/v1/sys/init".to_string(),
            headers: vec![],
            body: Some(serde_json::to_string(&self.config)?),
        };

        let body = assert_okay_with_body(self.rest_client.execute(&req).await?)?;

        match self.parser.parse(body) {
            Ok(json) => {
                let response: Resposne = from_value(json)?;
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
