use file_system::{
    EntryKind, FileReader, FileSystemError, FileWriter, Folder, UnixFileReader, UnixFileWriter,
    UnixFolder, encoding,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ApplicationDefinitionRepositryError {
    #[error("File system error: {0}")]
    FileSystemError(#[from] FileSystemError),

    #[error("Deserialization error: {0}")]
    Deserialization(#[from] toml::de::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] toml::ser::Error),

    #[error("No definition for '{0}'")]
    NoDefnition(String),

    #[error("Name cannot be empty")]
    NameCannotBeEmpty,

    #[error("Unknown dependency")]
    UnknownDependency(String),

    #[error("Environment variable name cannot be empty")]
    EnvironmentVariableNameCannotBeEmpty,

    #[error("Invalid environment variable name: '{0}'")]
    InvalidEnvironmentVariableName(String),

    #[error("Unknown dependency in share: mount: '{mount_name}', dependency: '{dependency}'")]
    UnknownDependencyInShare {
        mount_name: String,
        dependency: String,
    },

    #[error("File name cannot be empty")]
    FileNameCannotBeEmpty { mount_name: String },

    #[error("Unsafe file name")]
    UnsafeFileName {
        mount_name: String,
        file_name: String,
    },
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub enum ShareAccess {
    Readonly,
    ReadWrite,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct ShareDefinition {
    pub application_display_name: String,
    pub access: ShareAccess,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct MountFile {
    pub relative_mount_path: PathBuf,
    pub name: String,
    pub contents: String,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct MountDefinition {
    pub name: String,
    pub container_path: PathBuf,
    pub shared_with: Vec<ShareDefinition>,
    pub files: Vec<MountFile>,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub command: String,
    pub interval_in_seconds: u8,
    pub timeout_in_seconds: u8,
    pub start_period_in_seconds: u8,
    pub retries: u8,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ApplicationDefinition {
    pub display_name: String,
    #[serde(with = "image_name_serde")]
    pub image_name: docker::ImageName,
    pub dependencies: Vec<String>,
    #[serde(with = "environment_variables_serde")]
    pub environment_variables: Vec<docker::EnvironmentVariable>,
    pub mounts: Vec<MountDefinition>,
    pub health_check: Option<HealthCheck>,
}

mod image_name_serde {
    use docker::ImageName;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &ImageName, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!(
            "{}/{}:{}",
            value.namespace, value.name, value.version
        ))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<ImageName, D::Error> {
        let text = String::deserialize(d)?;

        let (namespace, name_and_version) = text.split_once("/").unwrap_or(("", &text));
        let (name, raw_version) = name_and_version.split_once(":").ok_or_else(|| {
            serde::de::Error::custom(format!("expected namespace/image:version, got: {text}"))
        })?;

        let namespace = namespace.trim();
        let name = name.trim();
        if name.is_empty() {
            return Err(serde::de::Error::custom(
                "Image name cannot be blank".to_string(),
            ));
        }

        let version: docker::Version = match raw_version.parse() {
            Ok(version) => version,
            Err(err) => return Err(serde::de::Error::custom(err.to_string())),
        };

        Ok(ImageName {
            namespace: namespace.to_string(),
            name: name.to_string(),
            version,
        })
    }
}

mod environment_variables_serde {
    use serde::ser::Error;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(
        value: &[docker::EnvironmentVariable],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let mut map = BTreeMap::new();

        for environment_variable in value {
            let name = environment_variable.name.trim();
            if name.is_empty() {
                return Err(S::Error::custom("Variable names cannot be empty"));
            }
            map.insert(name, &environment_variable.value);
        }

        map.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<docker::EnvironmentVariable>, D::Error> {
        let map = BTreeMap::<String, String>::deserialize(d)?;
        let mut result = Vec::new();

        for (name, value) in map {
            let name = name.trim();
            if name.is_empty() {
                return Err(serde::de::Error::custom("Variable names cannot be empty"));
            }
            result.push(docker::EnvironmentVariable::new(name, &value))
        }

        Ok(result)
    }
}

pub trait ApplicationDefinitionRepositry {
    fn list(&self) -> Result<Vec<ApplicationDefinition>, ApplicationDefinitionRepositryError>;
    fn exists(&self, display_name: &str) -> bool;
    fn load(
        &self,
        display_name: &str,
    ) -> Result<ApplicationDefinition, ApplicationDefinitionRepositryError>;
    fn save(
        &self,
        definition: &ApplicationDefinition,
    ) -> Result<(), ApplicationDefinitionRepositryError>;
}

pub struct FileApplicationDefinitionRepositry {
    root: PathBuf,
    folder: Box<dyn Folder>,
    file_reader: Box<dyn FileReader>,
    file_writer: Box<dyn FileWriter>,
}

impl FileApplicationDefinitionRepositry {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root: root.clone(),
            folder: Box::new(UnixFolder::new()),
            file_reader: Box::new(UnixFileReader::new()),
            file_writer: Box::new(UnixFileWriter::new()),
        }
    }

    fn qualified_path(&self, filename: &str) -> PathBuf {
        let mut path = self.root.clone();
        path.push(filename);
        path
    }

    fn load_definition(
        &self,
        path: PathBuf,
    ) -> Result<ApplicationDefinition, ApplicationDefinitionRepositryError> {
        let unparsed = self.file_reader.read_all(&path)?;
        let definition: ApplicationDefinition = toml::from_str(&unparsed)?;
        Ok(definition)
    }

    fn qualified_path_for_display_name(&self, display_name: &str) -> PathBuf {
        let filename = encoding::safe_file_system_name(display_name);
        self.qualified_path(&format!("{filename}.toml"))
    }

    fn validate(
        &self,
        definition: &ApplicationDefinition,
    ) -> Result<(), ApplicationDefinitionRepositryError> {
        self.assert_valid_name(&definition.display_name)?;
        self.assert_dependencies_exist(&definition.dependencies)?;
        self.assert_valid_environment_variables(&definition.environment_variables)?;
        self.assert_valid_mounts(&definition.mounts)?;
        Ok(())
    }

    fn assert_valid_name(&self, name: &str) -> Result<(), ApplicationDefinitionRepositryError> {
        if name.is_empty() {
            return Err(ApplicationDefinitionRepositryError::NameCannotBeEmpty);
        }
        Ok(())
    }

    fn assert_dependencies_exist(
        &self,
        dependencies: &[String],
    ) -> Result<(), ApplicationDefinitionRepositryError> {
        for dependency in dependencies {
            if !self.exists(dependency) {
                return Err(ApplicationDefinitionRepositryError::UnknownDependency(
                    dependency.to_string(),
                ));
            }
        }
        Ok(())
    }

    fn assert_valid_environment_variables(
        &self,
        environment_variables: &[docker::EnvironmentVariable],
    ) -> Result<(), ApplicationDefinitionRepositryError> {
        for environment_variable in environment_variables {
            let name = environment_variable.name.trim();
            if name.is_empty() {
                return Err(
                    ApplicationDefinitionRepositryError::EnvironmentVariableNameCannotBeEmpty,
                );
            }
            if name.contains("=") {
                return Err(
                    ApplicationDefinitionRepositryError::InvalidEnvironmentVariableName(
                        name.to_string(),
                    ),
                );
            }
        }
        Ok(())
    }

    fn assert_valid_mounts(
        &self,
        mounts: &[MountDefinition],
    ) -> Result<(), ApplicationDefinitionRepositryError> {
        for mount in mounts {
            for share in &mount.shared_with {
                self.assert_valid_mount_share(mount, share)?;
            }
            for file in &mount.files {
                self.assert_valid_file(mount, file)?;
            }
        }
        Ok(())
    }

    fn assert_valid_mount_share(
        &self,
        mount_definition: &MountDefinition,
        share: &ShareDefinition,
    ) -> Result<(), ApplicationDefinitionRepositryError> {
        if self.exists(&share.application_display_name) {
            return Ok(());
        }

        Err(
            ApplicationDefinitionRepositryError::UnknownDependencyInShare {
                mount_name: mount_definition.name.to_string(),
                dependency: share.application_display_name.to_string(),
            },
        )
    }

    fn assert_valid_file(
        &self,
        mount: &MountDefinition,
        file: &MountFile,
    ) -> Result<(), ApplicationDefinitionRepositryError> {
        let name = file.name.trim();
        if name.is_empty() {
            return Err(ApplicationDefinitionRepositryError::FileNameCannotBeEmpty {
                mount_name: mount.name.to_string(),
            });
        }
        let safe_name = encoding::safe_file_system_name(name);
        if name != safe_name {
            return Err(ApplicationDefinitionRepositryError::UnsafeFileName {
                mount_name: mount.name.to_string(),
                file_name: name.to_string(),
            });
        }

        Ok(())
    }
}

impl ApplicationDefinitionRepositry for FileApplicationDefinitionRepositry {
    fn list(&self) -> Result<Vec<ApplicationDefinition>, ApplicationDefinitionRepositryError> {
        let mut result = Vec::new();

        for entry in self.folder.entries(&self.root)? {
            if entry.kind == EntryKind::File && entry.name.ends_with(".toml") {
                let path = self.qualified_path(&entry.name);
                let definition = self.load_definition(path)?;
                result.push(definition);
            }
        }

        Ok(result)
    }

    fn load(
        &self,
        display_name: &str,
    ) -> Result<ApplicationDefinition, ApplicationDefinitionRepositryError> {
        let path = self.qualified_path_for_display_name(display_name);

        if !self.folder.exists(&path) {
            return Err(ApplicationDefinitionRepositryError::NoDefnition(
                display_name.to_string(),
            ));
        }
        let definition = self.load_definition(path)?;
        self.validate(&definition)?;
        Ok(definition)
    }

    fn save(
        &self,
        definition: &ApplicationDefinition,
    ) -> Result<(), ApplicationDefinitionRepositryError> {
        self.validate(definition)?;
        let path = self.qualified_path_for_display_name(&definition.display_name);
        let contents = toml::to_string(&definition)?;
        self.file_writer.write_all(&path, &contents)?;

        Ok(())
    }

    fn exists(&self, display_name: &str) -> bool {
        let path = self.qualified_path_for_display_name(display_name);
        self.folder.exists(&path)
    }
}
