use std::{collections::HashSet, sync::Arc};

use os::{EnvironmentVariableReader, UnixEnvironmentVariableReader};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum DouglasFlag {
    BractOnly,
}

pub trait DouglasFlagsReader {
    fn read(&self) -> HashSet<DouglasFlag>;
}

pub struct EnvironmentVariableDouglasFlagsReader {
    environment_variable_reader: Arc<dyn EnvironmentVariableReader>,
}

impl EnvironmentVariableDouglasFlagsReader {
    pub fn environment_variable_name() -> String {
        "DOUGFLAGS".to_string()
    }

    pub fn new(environment_variable_reader: Arc<dyn EnvironmentVariableReader>) -> Self {
        Self {
            environment_variable_reader,
        }
    }
    pub fn serialize(flags: &Vec<DouglasFlag>) -> String {
        serde_json::to_string(flags).unwrap_or_default()
    }
}

impl Default for EnvironmentVariableDouglasFlagsReader {
    fn default() -> Self {
        EnvironmentVariableDouglasFlagsReader::new(Arc::new(UnixEnvironmentVariableReader::new()))
    }
}

impl DouglasFlagsReader for EnvironmentVariableDouglasFlagsReader {
    fn read(&self) -> HashSet<DouglasFlag> {
        self.environment_variable_reader
            .read(&Self::environment_variable_name())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }
}
