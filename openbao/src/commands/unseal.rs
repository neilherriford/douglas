use crate::{OpenBaoError, Secret};
use log::Logger;
use rand::prelude::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::from_value;
use simple_rest_client::{
    Request, RestClient,
    assertions::assert_okay_with_body,
    parsers::{Parser, json::JsonParser},
};
use std::sync::Arc;

#[derive(Debug, Deserialize, PartialEq)]
struct Response {
    #[serde(rename = "t")]
    threshold: u8,
    #[serde(rename = "n")]
    share_count: u8,
    progress: u8,
    sealed: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ErrorResponse {
    errors: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
struct UnsealRequest {
    key: String,
}

pub struct UnsealCommand<'a> {
    rest_client: &'a mut dyn RestClient,
    parser: &'a JsonParser,
    secrets: Vec<Secret>,
    log: Arc<dyn Logger>,
}

impl<'a> UnsealCommand<'a> {
    pub fn new(
        rest_client: &'a mut dyn RestClient,
        parser: &'a JsonParser,
        secrets: Vec<Secret>,
        log: Arc<dyn Logger>,
    ) -> Self {
        let mut rng = rand::rng();
        let mut secrets = secrets.clone();
        secrets.shuffle(&mut rng);

        Self {
            rest_client,
            parser,
            secrets,
            log,
        }
    }

    pub async fn perform(&mut self) -> Result<(), OpenBaoError> {
        let mut secrets = self.secrets.clone();
        let mut key_attempt = 1;

        loop {
            let secret = secrets.pop().ok_or(OpenBaoError::InsufficentSecrets)?;

            self.log.info(&format!("Applying key #{key_attempt}…"));

            let response = self.unseal(secret.key).await?;
            if !response.sealed {
                break;
            }
            key_attempt += 1
        }

        self.log.info("OpenBao is unsealed!");

        Ok(())
    }

    async fn unseal(&mut self, key: String) -> Result<Response, OpenBaoError> {
        let req = Request::Post {
            path: "/v1/sys/unseal".to_string(),
            body: Some(serde_json::to_string(&UnsealRequest { key })?),
            headers: vec![],
        };

        let resposne = self.rest_client.execute(&req).await?;

        if let simple_rest_client::Response::Error { body, .. } = resposne {
            if let Some(body) = body {
                let parsed: ErrorResponse = self.parse(body)?;
                return Err(OpenBaoError::UnsealError(parsed.errors));
            } else {
                return Err(OpenBaoError::UnsealError(vec![
                    "No error context".to_string(),
                ]));
            }
        }

        let body = assert_okay_with_body(resposne)?;
        self.parse(body)
    }

    fn parse<T>(&self, body: String) -> Result<T, OpenBaoError>
    where
        T: DeserializeOwned,
    {
        match self.parser.parse(body) {
            Ok(json) => Ok(from_value(json)?),
            Err(err) => Err(OpenBaoError::ParseError {
                line: 0,
                column: 0,
                message: format!("{err}"),
            }),
        }
    }
}
