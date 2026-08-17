use serde::{Deserialize, Serialize};

pub static SOCKET_NAME: &str = "bract-trigger";

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Accepted,
    InvalidName,
}
