use crate::OpenBaoError;
use serde::Serialize;
use simple_rest_client::{Header, Request, RestClient, assertions::assert_no_content};
use std::collections::HashSet;

#[derive(Debug, PartialEq)]
pub enum Path {
    All(String),
    Explicit(String),
}

impl std::fmt::Display for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Path::All(path) => f.write_str(&format!("{path}/*")),
            Path::Explicit(path) => f.write_str(path),
        }
    }
}

#[derive(Debug)]
pub struct Policy {
    pub path: Path,
    pub capabilities: HashSet<Capability>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    Create,
    Read,
    Update,
    Delete,
    List,
    Sudo,
}

#[derive(Debug)]
struct PolicyList<'a>(&'a [Policy]);

impl<'a> Serialize for PolicyList<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let result = self
            .0
            .iter()
            .map(|policy| {
                {
                    format!(
                        "path \"{}\" {{ capabilities = [{}]}}",
                        policy.path,
                        policy
                            .capabilities
                            .iter()
                            .map(|c| {
                                let value = match c {
                                    Capability::Create => "create",
                                    Capability::Read => "read",
                                    Capability::Update => "update",
                                    Capability::Delete => "delete",
                                    Capability::List => "list",
                                    Capability::Sudo => "sudo",
                                };
                                format!("\"{value}\"")
                            })
                            .collect::<Vec<String>>()
                            .join(",")
                    )
                }
            })
            .collect::<Vec<String>>()
            .join("\n");

        serializer.serialize_str(&result)
    }
}

#[derive(Debug, Serialize)]
struct Body<'a> {
    policy: &'a PolicyList<'a>,
}

pub(crate) struct UpsertAclPolicy<'a> {
    rest_client: &'a mut dyn RestClient,
    name: &'a str,
    polices: &'a [Policy],
    vault_token: &'a str,
}

impl<'a> UpsertAclPolicy<'a> {
    pub fn new(
        rest_client: &'a mut dyn RestClient,
        vault_token: &'a str,
        name: &'a str,
        polices: &'a [Policy],
    ) -> Self {
        Self {
            rest_client,
            vault_token,
            name,
            polices,
        }
    }

    pub async fn perform(&mut self) -> Result<(), OpenBaoError> {
        let req = Request::Post {
            path: format!("/v1/sys/policies/acl/{}", self.name),
            headers: vec![Header::new("X-Vault-Token", self.vault_token)],
            body: Some(serde_json::to_string(&Body {
                policy: &PolicyList(self.polices),
            })?),
        };

        let response = self.rest_client.execute(&req).await?;
        assert_no_content(response)?;

        Ok(())
    }
}
