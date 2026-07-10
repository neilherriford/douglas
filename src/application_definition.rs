use docker::{Capability, EnvironmentVariable, Label, VersionedImageName};
use std::{collections::HashMap, path::PathBuf};

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct MountFile {
    pub relative_path: PathBuf,
    pub contents: String,
    pub template_variables: HashMap<String, String>,
}

impl MountFile {
    pub fn in_root(name: &str, contents: &str, template_variables: Vec<(&str, &str)>) -> Self {
        let mut relative_path = PathBuf::from(".");
        relative_path.push(name);

        Self {
            relative_path,
            contents: contents.into(),
            template_variables: template_variables
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct MountTemplate {
    pub name: String,
    pub container_path: PathBuf,
    pub writable: bool,
    pub files: Vec<MountFile>,
    pub shared: bool,
}

impl MountTemplate {
    pub fn empty_mount(name: &str, container_path: &str) -> Self {
        Self {
            name: name.to_string(),
            container_path: PathBuf::from(container_path),
            writable: true,
            files: vec![],
            shared: false,
        }
    }

    pub fn shared_empty_mount(name: &str, container_path: &str) -> Self {
        Self {
            name: name.to_string(),
            container_path: PathBuf::from(container_path),
            writable: true,
            files: vec![],
            shared: true,
        }
    }

    pub fn populated_mount(name: &str, container_path: &str, files: Vec<MountFile>) -> Self {
        Self {
            name: name.to_string(),
            container_path: PathBuf::from(container_path),
            writable: true,
            files,
            shared: false,
        }
    }
}

pub trait ApplicationDefinition {
    fn name(&self) -> String;
    fn image_name(&self) -> VersionedImageName;
    fn command(&self) -> Option<String>;
    fn environment_variables(&self) -> Vec<EnvironmentVariable>;
    fn added_capabilities(&self) -> Vec<Capability>;
    fn mount_templates(&self) -> Vec<MountTemplate>;
    fn labels(&self) -> Vec<Label>;
}
