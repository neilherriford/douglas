use docker::{Capability, EnvironmentVariable, ImageName, Label};
use std::path::PathBuf;

pub struct MountFile {
    pub relative_path: PathBuf,
    pub contents: String,
}

impl MountFile {
    pub fn in_root(name: &str, contents: &str) -> Self {
        let mut relative_path = PathBuf::from(".");
        relative_path.push(name);

        Self {
            relative_path,
            contents: contents.into(),
        }
    }
}

pub struct MountTemplate {
    pub name: String,
    pub container_path: PathBuf,
    pub writable: bool,
    pub files: Vec<MountFile>,
}

pub struct ApplicationDefinition {
    pub name: String,
    pub image_name: ImageName,
    pub command: Option<String>,
    pub environment_variables: Vec<EnvironmentVariable>,
    pub added_capabilities: Vec<Capability>,
    pub mount_templates: Vec<MountTemplate>,
    pub labels: Vec<Label>,
}

impl ApplicationDefinition {
    pub fn new(name: &str, image_name: ImageName) -> Self {
        Self {
            name: name.into(),
            image_name,
            command: None,
            environment_variables: vec![],
            added_capabilities: vec![],
            mount_templates: vec![],
            labels: vec![],
        }
    }

    pub fn with_command(mut self, command: &str) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn with_environment_variable(mut self, name: &str, value: &str) -> Self {
        self.environment_variables
            .push(EnvironmentVariable::new(name, value));
        self
    }

    pub fn with_empty_mount(self, name: &str, container_path: &str) -> Self {
        self.with_mount(name, container_path, vec![])
    }

    pub fn with_mount(mut self, name: &str, container_path: &str, files: Vec<MountFile>) -> Self {
        self.mount_templates.push(MountTemplate {
            name: name.into(),
            container_path: PathBuf::from(container_path),
            writable: true,
            files,
        });
        self
    }

    pub fn with_capability(mut self, capability: Capability) -> Self {
        if !self.added_capabilities.contains(&capability) {
            self.added_capabilities.push(capability);
        }
        self
    }

    pub fn with_label(mut self, name: &str, value: &str) -> Self {
        self.labels.push(Label::new(name, value));
        self
    }
}
