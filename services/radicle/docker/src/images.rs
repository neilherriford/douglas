use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyperlocal::{UnixClientExt, UnixConnector, Uri};
use std::error::Error;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct Tag {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct Image {
    #[serde(rename = "Id")]
    pub id: String,

    #[serde(rename = "RepoTags")]
    #[serde(deserialize_with = "deserialize_tags")]
    pub tags: Vec<Tag>,
}

fn deserialize_tags<'de, D>(deserializer: D) -> Result<Vec<Tag>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw_strings: Vec<String> = Deserialize::deserialize(deserializer)?;

    let tags = raw_strings
        .into_iter()
        .map(|raw_tag| {
            let parts: Vec<&str> = raw_tag.split(':').collect();
            let (name, version) = match parts.as_slice() {
                [first, second] => (first.to_string(), second.to_string()),
                _ => (raw_tag, String::from("")),
            };
            Tag { name, version }
        })
        .collect();

    Ok(tags)
}

pub async fn list() -> Result<Vec<Image>, Box<dyn Error + Send + Sync>> {
    let url = Uri::new("/var/run/docker.sock", "/images/json").into();

    let client: Client<UnixConnector, Full<Bytes>> = Client::unix();

    let mut response = client.get(url).await?;
    let mut body = String::from("");

    while let Some(frame_result) = response.frame().await {
        let frame = frame_result?;

        if let Some(segment) = frame.data_ref() {
            let segment_contents = std::str::from_utf8(segment.iter().as_slice()).unwrap();
            body.push_str(segment_contents);
        }
    }

    let data: Value = serde_json::from_str(&body).expect("Failed to parse JSON");

    let images: Vec<Image> = data
        .as_array()
        .unwrap()
        .iter()
        .map(|obj| serde_json::from_value(obj.clone()).expect("Failed to deserialize"))
        .collect();

    Ok(images)
}
