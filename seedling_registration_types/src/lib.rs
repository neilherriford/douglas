use serde::{Deserialize, Serialize};

pub static SOCKET_NAME: &str = "seedbank-registration";

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Registered,
    NotRegistered,
    InvalidName,
    Reserved,
}
