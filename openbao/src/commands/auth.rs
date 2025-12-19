use crate::{AuthType, OpenBaoError, commands::utils::headers::root_token};
use serde::Serialize;
use simple_rest_client::{
    Request, RestClient, assertions::assert_no_content, parsers::json::JsonParser,
};

impl AuthType {
    fn mount_name(&self) -> String {
        match self {
            AuthType::Certificate => "cert".into(),
        }
    }
}

impl Serialize for AuthType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            AuthType::Certificate => serializer.serialize_str("cert"),
        }
    }
}

pub struct IsInstalledCommand<'a> {
    rest_client: &'a mut dyn RestClient,
    root_token: String,
    auth_type: AuthType,
}

impl<'a> IsInstalledCommand<'a> {
    pub fn new(
        rest_client: &'a mut dyn RestClient,
        root_token: String,
        auth_type: AuthType,
    ) -> Self {
        Self {
            rest_client,
            root_token,
            auth_type,
        }
    }

    pub async fn perform(&mut self) -> Result<bool, OpenBaoError> {
        let req = Request::Get {
            path: format!("/v1/sys/auth/{}", self.auth_type.mount_name()),
            headers: vec![root_token(&self.root_token)],
        };

        match self.rest_client.execute(&req).await? {
            simple_rest_client::Response::Okay { .. } => Ok(true),
            simple_rest_client::Response::Created { headers, body } => {
                Err(OpenBaoError::UnexpectedResponse {
                    status: 201,
                    body,
                    message: "expected OK, but recieved CREATED".to_string(),
                })
            }
            simple_rest_client::Response::NoContent { headers } => {
                Err(OpenBaoError::UnexpectedResponse {
                    status: 204,
                    body: None,
                    message: "expected OK, but recieved NO CONTENT".to_string(),
                })
            }
            simple_rest_client::Response::Error { status, .. } if status == 400 => Ok(false),
            simple_rest_client::Response::Error {
                headers,
                status,
                body,
            } => Err(OpenBaoError::UnexpectedResponse {
                status: 204,
                body,
                message: "unexpected error".to_string(),
            }),
        }
    }
}

#[derive(Debug, PartialEq, Serialize)]
struct AuthRequest {
    #[serde(rename = "type")]
    auth_type: AuthType,
    description: String,
}

pub struct InstallAuthCommand<'a> {
    rest_client: &'a mut dyn RestClient,
    root_token: String,
    auth_type: AuthType,
    decription: String,
}

impl<'a> InstallAuthCommand<'a> {
    pub fn new(
        rest_client: &'a mut dyn RestClient,
        root_token: String,
        auth_type: AuthType,
        decription: &str,
    ) -> Self {
        Self {
            rest_client,
            root_token,
            auth_type,
            decription: decription.into(),
        }
    }

    pub async fn perform(&mut self) -> Result<(), OpenBaoError> {
        let req = Request::Post {
            path: format!("/v1/sys/auth/{}", self.auth_type.mount_name()),
            headers: vec![root_token(&self.root_token)],
            body: Some(serde_json::to_string(&AuthRequest {
                auth_type: self.auth_type.clone(),
                description: self.decription.clone(),
            })?),
        };

        assert_no_content(self.rest_client.execute(&req).await?)?;
        Ok(())
    }
}
