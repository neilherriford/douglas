use crate::{Error, Name, Seedbank, Seedling, SeedlingDefinition};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    List,
    Exists {
        name: Name,
    },
    Load {
        name: Name,
    },
    Create {
        name: Name,
        definition: SeedlingDefinition,
    },
    Delete {
        name: Name,
    },
    Update {
        name: Name,
        definition: SeedlingDefinition,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    Names { names: Vec<Name> },
    Exists { exists: bool },
    Seedling { seedling: Seedling },
    Ok,
    Error { message: String },
}

pub fn handle(seedbank: &dyn Seedbank, request: Request) -> Response {
    match request {
        Request::List => match seedbank.list() {
            Ok(names) => Response::Names { names },
            Err(err) => error_response(err),
        },
        Request::Exists { name } => match seedbank.exists(&name) {
            Ok(exists) => Response::Exists { exists },
            Err(err) => error_response(err),
        },
        Request::Load { name } => match seedbank.load(&name) {
            Ok(seedling) => Response::Seedling { seedling },
            Err(err) => error_response(err),
        },
        Request::Create { name, definition } => match seedbank.create(&name, &definition) {
            Ok(()) => Response::Ok,
            Err(err) => error_response(err),
        },
        Request::Delete { name } => match seedbank.delete(&name) {
            Ok(()) => Response::Ok,
            Err(err) => error_response(err),
        },
        Request::Update { name, definition } => match seedbank.update(&name, &definition) {
            Ok(()) => Response::Ok,
            Err(err) => error_response(err),
        },
    }
}

fn error_response(err: Error) -> Response {
    Response::Error {
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Id, MockSeedbank};
    use docker_types::VersionedImageName;
    use std::str::FromStr;

    fn name(value: &str) -> Name {
        Name::from_str(value).expect("valid name")
    }

    fn id(value: u16) -> Id {
        Id { value }
    }

    fn definition() -> SeedlingDefinition {
        SeedlingDefinition::new(VersionedImageName::latest("test"))
    }

    #[test]
    fn test_list_should_wrap_success_in_names() {
        let mut seedbank = MockSeedbank::new();
        seedbank.expect_list().returning(|| Ok(vec![name("foo")]));

        let response = handle(&seedbank, Request::List);

        assert!(matches!(response, Response::Names { names } if names == vec![name("foo")]));
    }

    #[test]
    fn test_list_should_wrap_failure_in_error() {
        let mut seedbank = MockSeedbank::new();
        seedbank
            .expect_list()
            .returning(|| Err(Error::CannotBeRoot));

        let response = handle(&seedbank, Request::List);

        assert!(matches!(response, Response::Error { .. }));
    }

    #[test]
    fn test_exists_should_dispatch_with_name_and_wrap_bool() {
        let mut seedbank = MockSeedbank::new();
        seedbank
            .expect_exists()
            .withf(|checked| checked == &name("foo"))
            .returning(|_| Ok(true));

        let response = handle(&seedbank, Request::Exists { name: name("foo") });

        assert!(matches!(response, Response::Exists { exists: true }));
    }

    #[test]
    fn test_load_should_wrap_success_in_seedling() {
        let mut seedbank = MockSeedbank::new();
        seedbank
            .expect_load()
            .withf(|loaded| loaded == &name("foo"))
            .returning(|name| {
                Ok(Seedling {
                    id: id(0),
                    name: name.clone(),
                    definition: definition(),
                })
            });

        let response = handle(&seedbank, Request::Load { name: name("foo") });

        assert!(matches!(
            response,
            Response::Seedling { seedling } if seedling.name == name("foo")
        ));
    }

    #[test]
    fn test_create_should_dispatch_with_name_and_definition_and_wrap_ok() {
        let mut seedbank = MockSeedbank::new();
        seedbank
            .expect_create()
            .withf(|created, _definition| created == &name("foo"))
            .returning(|_, _| Ok(()));

        let response = handle(
            &seedbank,
            Request::Create {
                name: name("foo"),
                definition: definition(),
            },
        );

        assert!(matches!(response, Response::Ok));
    }

    #[test]
    fn test_delete_should_dispatch_with_name_and_wrap_ok() {
        let mut seedbank = MockSeedbank::new();
        seedbank
            .expect_delete()
            .withf(|deleted| deleted == &name("foo"))
            .returning(|_| Ok(()));

        let response = handle(&seedbank, Request::Delete { name: name("foo") });

        assert!(matches!(response, Response::Ok));
    }

    #[test]
    fn test_update_should_dispatch_with_name_and_definition_and_wrap_ok() {
        let mut seedbank = MockSeedbank::new();
        seedbank
            .expect_update()
            .withf(|updated, _definition| updated == &name("foo"))
            .returning(|_, _| Ok(()));

        let response = handle(
            &seedbank,
            Request::Update {
                name: name("foo"),
                definition: definition(),
            },
        );

        assert!(matches!(response, Response::Ok));
    }
}
