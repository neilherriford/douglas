use crate::{Error, Server};
use bract_types::{Request, Response};
use log::Reporter;
use std::sync::Arc;

pub async fn handle(server: &dyn Server, reporter: Arc<dyn Reporter>, request: Request) -> Response {
    match request {
        Request::SeedlingStatus { name } => {
            match server.seedling_status(reporter, &name).await {
                Ok(status) => Response::SeedlingStatus(status),
                Err(err) => error_response(err),
            }
        }
        Request::StartSeedling { name } => match server.start_seedling(reporter, &name).await {
            Ok(()) => Response::Started,
            Err(err) => error_response(err),
        },
        Request::StopSeedling { name } => match server.stop_seedling(reporter, &name).await {
            Ok(()) => Response::Stopped,
            Err(err) => error_response(err),
        },
        Request::DropSeedling { name } => match server.drop_seedling(reporter, &name).await {
            Ok(()) => Response::Dropped,
            Err(err) => error_response(err),
        },
        Request::ReconcileSeedling {
            name,
            version,
            seedling_definition,
        } => match server
            .reconcile_seedling(reporter, &name, &version, &seedling_definition)
            .await
        {
            Ok(()) => Response::Started,
            Err(err) => error_response(err),
        },
    }
}

fn error_response(err: Error) -> Response {
    Response::Error {
        message: err.to_string(),
    }
}
