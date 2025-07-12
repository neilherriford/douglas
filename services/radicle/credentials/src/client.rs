use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum Request {
    CreateDirectoryEntry { token: String, service_name: String },
}
