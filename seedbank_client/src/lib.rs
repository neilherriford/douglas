use async_trait::async_trait;
use config::DouglasFolders;
use log::{Reporter, ScopeKind, Span};
#[cfg(feature = "mock")]
use mockall::automock;
use seedbank_types::{
    DesiredRunStatus, HealthCheckLog, Name, Request, Response, Seedling, SeedlingDefinition,
    SeedlingStatus, Version,
};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Missing socket. Has seedbank been started?")]
    MissingSocket,
    #[error("Connection refused. Is seedbank running?")]
    ConnectionRefused,
    #[error("Server closed the connection without responding")]
    NoResponse,
    #[error("Failed to serialize request: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("IO error: {0}")]
    IoError(std::io::Error),
    #[error("Server error: {0}")]
    ServerError(String),
    #[error("Unexpected response")]
    UnexpectedResponse,
}

#[cfg_attr(feature = "mock", automock)]
#[async_trait]
pub trait Client: Send + Sync {
    async fn list(&self) -> Result<Vec<Name>, Error>;
    async fn status(&self, name: &Name) -> Result<SeedlingStatus, Error>;
    async fn exists(&self, name: &Name) -> Result<bool, Error>;
    async fn load(&self, name: &Name) -> Result<Seedling, Error>;
    async fn create(
        &self,
        name: &Name,
        version: &Version,
        definition: &SeedlingDefinition,
    ) -> Result<(), Error>;
    async fn delete(&self, name: &Name) -> Result<(), Error>;
    async fn update(
        &self,
        name: &Name,
        version: &Version,
        definition: &SeedlingDefinition,
    ) -> Result<(), Error>;
    async fn default_seedling(&self) -> Result<Option<Name>, Error>;
    async fn claim_default(&self, name: &Name) -> Result<(), Error>;
    async fn release_default(&self, name: &Name) -> Result<(), Error>;
    async fn get_desired_run_status(&self, name: &Name) -> Result<DesiredRunStatus, Error>;
    async fn set_desired_run_status(
        &self,
        name: &Name,
        desired_run_status: DesiredRunStatus,
    ) -> Result<(), Error>;
    async fn reset_health_log(&self, name: &Name) -> Result<(), Error>;
    async fn health_check_log(&self, name: &Name) -> Result<Option<HealthCheckLog>, Error>;
    async fn increment_health_log_fail_count(&self, name: &Name) -> Result<bool, Error>;
}

pub struct UdsClient {
    reporter: Arc<dyn Reporter>,
    socket_path: PathBuf,
}

impl UdsClient {
    pub fn new(reporter: Arc<dyn Reporter>, douglas_folders: &DouglasFolders) -> Self {
        Self {
            reporter,
            socket_path: douglas_folders.socket_file("seedbank"),
        }
    }

    async fn request(&self, request: Request) -> Result<Response, Error> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(classify_connect_error)?;

        let mut serialized = serde_json::to_string(&request)?;
        serialized.push('\n');

        let (reader, mut writer) = stream.split();
        writer
            .write_all(serialized.as_bytes())
            .await
            .map_err(Error::IoError)?;

        let mut lines = BufReader::new(reader).lines();
        match lines.next_line().await.map_err(Error::IoError)? {
            Some(line) => Ok(serde_json::from_str(&line)?),
            None => Err(Error::NoResponse),
        }
    }
}

#[async_trait]
impl Client for UdsClient {
    async fn list(&self) -> Result<Vec<Name>, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Listing seedlings",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self.request(Request::List).await {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Names { names } => Ok(names),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn status(&self, name: &Name) -> Result<SeedlingStatus, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Checking seedling status",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self.request(Request::Status { name: name.clone() }).await {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Status { status } => Ok(status),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn exists(&self, name: &Name) -> Result<bool, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Checking seedling existence",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self.request(Request::Exists { name: name.clone() }).await {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Exists { exists } => Ok(exists),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn load(&self, name: &Name) -> Result<Seedling, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Loading seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self.request(Request::Load { name: name.clone() }).await {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Seedling { seedling } => Ok(*seedling),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn create(
        &self,
        name: &Name,
        version: &Version,
        definition: &SeedlingDefinition,
    ) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Creating seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(Request::Create {
                name: name.clone(),
                version: version.clone(),
                definition: definition.clone(),
            })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn delete(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Deleting seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self.request(Request::Delete { name: name.clone() }).await {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn update(
        &self,
        name: &Name,
        version: &Version,
        definition: &SeedlingDefinition,
    ) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Updating seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(Request::Update {
                name: name.clone(),
                version: version.clone(),
                definition: definition.clone(),
            })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn default_seedling(&self) -> Result<Option<Name>, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Checking default seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self.request(Request::Default).await {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Default { name } => Ok(name),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn claim_default(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Claiming default seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(Request::ClaimDefault { name: name.clone() })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn release_default(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Releasing default seedling",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(Request::ReleaseDefault { name: name.clone() })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn get_desired_run_status(&self, name: &Name) -> Result<DesiredRunStatus, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Getting desired run status",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(Request::GetDesiredRunStatus { name: name.clone() })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::DesiredRunStatus { desired_run_status } => Ok(desired_run_status),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn set_desired_run_status(
        &self,
        name: &Name,
        desired_run_status: DesiredRunStatus,
    ) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Setting desired run status",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(Request::SetDesiredRunStatus {
                name: name.clone(),
                desired_run_status,
            })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn reset_health_log(&self, name: &Name) -> Result<(), Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Resetting health log",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(Request::ResetHealthLog { name: name.clone() })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::Ok => Ok(()),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn health_check_log(&self, name: &Name) -> Result<Option<HealthCheckLog>, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Fetching health log status",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(Request::HealthCheckLog { name: name.clone() })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::HealthCheckLog { log } => Ok(log),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }

    async fn increment_health_log_fail_count(&self, name: &Name) -> Result<bool, Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Incrementing health log fail count",
            ScopeKind::Task,
        )
        .start_guard();

        let response = match self
            .request(Request::IncrementHealthLogFailCount { name: name.clone() })
            .await
        {
            Ok(response) => response,
            Err(err) => return guard.finish(Err(err)),
        };

        guard.finish(match response {
            Response::IncrementHealthLogFailCount {
                reached_max_fail_count,
            } => Ok(reached_max_fail_count),
            Response::Error { message } => Err(Error::ServerError(message)),
            _ => Err(Error::UnexpectedResponse),
        })
    }
}

fn classify_connect_error(err: std::io::Error) -> Error {
    match err.kind() {
        std::io::ErrorKind::NotFound => Error::MissingSocket,
        std::io::ErrorKind::ConnectionRefused => Error::ConnectionRefused,
        _ => Error::IoError(err),
    }
}
