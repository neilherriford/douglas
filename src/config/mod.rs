use file_system::{FileReader, FileSystemError, FileWriter, Permissions};
use serde::{Deserialize, Serialize};
use simple_rest_client::parsers::{
    Parser,
    json::{JsonParser, JsonParserError},
};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub(crate) open_bao: OpenBaoConfig,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenBaoConfig {
    pub(crate) admin_role_id: String,
    pub(crate) admin_role_secret: String,
}

#[derive(Error, Debug)]
pub enum ConfigStoreError {
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),

    #[error("Parse Error")]
    ParseError {
        line: usize,
        column: usize,
        message: String,
    },
}

impl From<serde_json::Error> for ConfigStoreError {
    fn from(err: serde_json::Error) -> ConfigStoreError {
        ConfigStoreError::ParseError {
            line: err.line(),
            column: err.column(),
            message: err.to_string(),
        }
    }
}

impl From<JsonParserError> for ConfigStoreError {
    fn from(err: JsonParserError) -> Self {
        match err {
            JsonParserError::Error {
                line,
                column,
                message,
            } => ConfigStoreError::ParseError {
                line,
                column,
                message,
            },
        }
    }
}

pub trait ConfigStore {
    fn exists(&self) -> bool;
    fn load(&self) -> Result<Config, ConfigStoreError>;
    fn save(&self, config: Config) -> Result<(), ConfigStoreError>;
}

pub struct LocalConfigStore<'a> {
    file_reader: &'a dyn FileReader,
    file_writer: &'a dyn FileWriter,
    permissions: &'a dyn Permissions,
    parser: &'a JsonParser,
    config_path: &'a Path,
}

impl<'a> LocalConfigStore<'a> {
    pub fn new(
        file_reader: &'a dyn FileReader,
        file_writer: &'a dyn FileWriter,
        permissions: &'a dyn Permissions,
        parser: &'a JsonParser,
        config_path: &'a Path,
    ) -> Self {
        Self {
            file_reader,
            file_writer,
            permissions,
            parser,
            config_path,
        }
    }
}

impl ConfigStore for LocalConfigStore<'_> {
    fn load(&self) -> Result<Config, ConfigStoreError> {
        let unparsed = self.file_reader.read_all(self.config_path)?;
        let json = self.parser.parse(unparsed)?;
        let parsed = serde_json::from_value::<Config>(json)?;

        Ok(parsed)
    }

    fn save(&self, config: Config) -> Result<(), ConfigStoreError> {
        let is_new_file = !self.file_writer.exists(self.config_path);
        let contents = serde_json::to_string(&config)?;
        self.file_writer.write_all(self.config_path, &contents)?;
        if is_new_file {
            self.permissions.change_mode(
                self.config_path,
                &file_system::Modes::OwnerReadWriteGroupRead,
            )?;
        }
        Ok(())
    }

    fn exists(&self) -> bool {
        self.file_reader.exists(self.config_path)
    }
}
