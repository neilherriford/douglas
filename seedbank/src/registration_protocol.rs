use crate::{Error, Name, Seedbank};
use seedling_registration_types::{Request, Response};

pub fn handle(seedbank: &dyn Seedbank, request: Request) -> Result<Response, Error> {
    let Ok(name) = request.name.parse::<Name>() else {
        return Ok(Response::InvalidName);
    };

    Ok(if seedbank.exists(&name)? {
        Response::Registered
    } else {
        Response::NotRegistered
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockSeedbank;

    #[test]
    fn test_handle_should_report_invalid_name() {
        let seedbank = MockSeedbank::new();

        let response = handle(
            &seedbank,
            Request {
                name: "Not Valid!".to_string(),
            },
        );

        assert!(matches!(response, Ok(Response::InvalidName)));
    }

    #[test]
    fn test_handle_should_report_registered_when_the_seedling_exists() {
        let mut seedbank = MockSeedbank::new();
        seedbank.expect_exists().returning(|_| Ok(true));

        let response = handle(
            &seedbank,
            Request {
                name: "traefik".to_string(),
            },
        );

        assert!(matches!(response, Ok(Response::Registered)));
    }

    #[test]
    fn test_handle_should_report_not_registered_when_the_seedling_is_missing() {
        let mut seedbank = MockSeedbank::new();
        seedbank.expect_exists().returning(|_| Ok(false));

        let response = handle(
            &seedbank,
            Request {
                name: "traefik".to_string(),
            },
        );

        assert!(matches!(response, Ok(Response::NotRegistered)));
    }

    #[test]
    fn test_handle_should_bubble_up_internal_errors() {
        let mut seedbank = MockSeedbank::new();
        seedbank
            .expect_exists()
            .returning(|_| Err(Error::CannotBeRoot));

        let response = handle(
            &seedbank,
            Request {
                name: "traefik".to_string(),
            },
        );

        assert!(response.is_err());
    }
}
