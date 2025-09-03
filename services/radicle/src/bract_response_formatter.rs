use bract::Response;
use mockall::automock;
use serde_json::{Value, json};
use std::fmt::Display;

#[automock]
pub trait BractResponseFormatter {
    fn format(&self, response: Response) -> String;
}

pub struct PlainBractResponseFormatter {}

enum Bullet {
    Circle,
    Triangle,
    Dash,
}

impl Display for Bullet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Bullet::Circle => write!(f, "• ")?,
            Bullet::Triangle => write!(f, "‣ ")?,
            Bullet::Dash => write!(f, "⁃ ")?,
        }
        Ok(())
    }
}

impl PlainBractResponseFormatter {
    pub fn new() -> Self {
        Self {}
    }
    fn create_bullet_line(&self, indent: u8, bullet: Bullet, line: &str) -> String {
        let mut indentation = String::new();
        for _ in 0..indent {
            indentation += "  ";
        }
        format!("{}{}{}\n", indentation, bullet, line)
    }
}

impl BractResponseFormatter for PlainBractResponseFormatter {
    fn format(&self, response: Response) -> String {
        match response {
            Response::CredentialsCreated { user, group } => {
                format!("Credentials created:\n  {}\n  {}", user, group)
            }
            Response::MountSet { version, path } => {
                format!("Mount set to {} ({:?})", version, path)
            }
            Response::MountVersionsListed(versions) => {
                let mut result = String::from("Mount versions:\n");
                for version in versions {
                    result += &self.create_bullet_line(2, Bullet::Circle, &version.to_string());
                }
                result.to_string()
            }
            Response::InvalidToken => "Invalid Token".to_string(),
            Response::Status {
                token_path,
                mount_root,
                services,
            } => {
                let mut result = format!(
                    "Status: OK\n  Token path: {:?}\n  Mount root: {:?}\n  Services:\n",
                    token_path, mount_root
                );
                for service in services {
                    result += &self.create_bullet_line(2, Bullet::Circle, &service.name);
                    for mount in service.mounts {
                        result += &self.create_bullet_line(3, Bullet::Triangle, &mount.name);
                        for version in mount.available {
                            let marker = if version == mount.active {
                                " Active"
                            } else {
                                ""
                            };

                            let version = version.to_string() + marker;
                            result += &self.create_bullet_line(4, Bullet::Dash, &version);
                        }
                    }
                }
                result
            }
            Response::Error(error) => format!("Error!\n{:?}", error),
            Response::ShuttingDown => "Shutting down".to_string(),
            Response::Stopped => "Stopped".to_string(),
        }
    }
}

pub struct JsonBractResponseFormatter {}

impl JsonBractResponseFormatter {
    pub fn new() -> Self {
        Self {}
    }
}

impl BractResponseFormatter for JsonBractResponseFormatter {
    fn format(&self, response: Response) -> String {
        let value = match response {
            Response::CredentialsCreated { user, group } => json!({
                "response": "CredentialsCreated",
                "data": json!({
                    "user": user,
                    "group": group,
                })
            }),
            Response::MountSet { version, path } => json!({
                "response": "MountSet",
                "data": json!({
                    "version": version.to_string(),
                    "path": path.to_string_lossy(),
                })
            }),
            Response::MountVersionsListed(versions) => json!({
                "response": "MountVersionsListed",
                "data": versions,
            }),
            Response::InvalidToken => json!({
                "response": "InvalidToken",
                "data": Value::Null,
            }),
            Response::Status {
                token_path,
                mount_root,
                services,
            } => json!({
                "response": "Status",
                "data": json!({
                    "token_path": token_path.to_string_lossy(),
                    "mount_root": mount_root.to_string_lossy(),
                    "services": services,
                })
            }),
            Response::Error(err) => json!({
                "response": "Error",
                "data": json!({
                    "message": format!("{:?}",err )
                })
            }),
            Response::ShuttingDown => json!({
                "response": "ShuttingDown",
                "data": Value::Null,
            }),
            Response::Stopped => json!({
                "response": "Stopped",
                "data": Value::Null,
            }),
        };
        match serde_json::to_string_pretty(&value) {
            Ok(result) => result,
            Err(err) => {
                let mut template = String::from(r#"{"response": "Error", "data": {"message": ""#);
                template += &format!("{:?}", err);
                template += r#""}}"#;
                template
            }
        }
    }
}
