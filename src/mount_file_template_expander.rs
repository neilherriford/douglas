use crate::application_definition::MountFile;
use bract::client::Credential;
use config::constants;
use credentials::create_credentials;
use os::Unix;
use std::{collections::HashMap, sync::Arc};

pub struct MountFileTemplateExpander {
    variables: HashMap<String, String>,
}

impl MountFileTemplateExpander {
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
        }
    }

    pub fn with_douglas_group(&mut self) -> &mut Self {
        let credentials = create_credentials(Arc::new(Unix::new()));
        if let Some(group_id) = credentials.get_group_id(constants::DOUGLAS_GROUP) {
            self.variables.insert(
                "douglas_group".to_string(),
                constants::DOUGLAS_GROUP.to_string(),
            );
            self.variables
                .insert("douglas_group_id".to_string(), group_id.to_string());
        }

        self
    }

    pub fn with_runas_credentail(&mut self, runas_credential: &Credential) -> &mut Self {
        self.variables
            .insert("runas_user".to_string(), runas_credential.user.clone());
        self.variables.insert(
            "runas_user_id".to_string(),
            runas_credential.user_id.to_string(),
        );
        self.variables
            .insert("runas_group".to_string(), runas_credential.group.clone());
        self.variables.insert(
            "runas_group_id".to_string(),
            runas_credential.group_id.to_string(),
        );

        self
    }

    pub fn expand(&self, mount_file: &MountFile) -> String {
        let mut result = String::new();
        let chars: Vec<char> = mount_file.contents.chars().collect();
        let mut index = 0;

        while index < chars.len() {
            if chars[index] == '$' && index + 1 < chars.len() && chars[index + 1] == '{' {
                let mut close_bracket_index = index + 2;
                while close_bracket_index < chars.len() && chars[close_bracket_index] != '}' {
                    close_bracket_index += 1;
                }

                if close_bracket_index < chars.len() {
                    let key: String = chars[index + 2..close_bracket_index].iter().collect();

                    let mut replacement = self.variables.get(key.as_str());
                    if replacement.is_none() {
                        replacement = mount_file.template_variables.get(key.as_str());
                    }

                    if let Some(value) = replacement {
                        result.push_str(value);
                        index = close_bracket_index + 1;
                        continue;
                    }
                }
            }
            result.push(chars[index]);
            index += 1;
        }

        result
    }
}
