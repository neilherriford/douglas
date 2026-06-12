use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use config::DouglasFolders;
use log::{BufferedFileReporter, Outcome, Reporter, ScopeKind, Span};
use std::sync::Arc;

pub struct Server {
    reporter: Arc<dyn Reporter>,
}

impl Default for Server {
    fn default() -> Self {
        let douglas_folders = DouglasFolders::new();
        let reporter: Arc<dyn Reporter> =
            Arc::new(BufferedFileReporter::new(douglas_folders.log_file("resin")));

        Server::new(reporter)
    }
}

impl Server {
    pub fn new(reporter: Arc<dyn Reporter>) -> Self {
        Self { reporter }
    }

    pub async fn start(&self) -> Result<(), std::io::Error> {
        let guard = Span::new(
            Arc::clone(&self.reporter),
            "Starting douglas system",
            log::ScopeKind::Group,
        )
        .start_guard();

        let app = Router::new()
            .route("/v2/", get(v2))
            .with_state(Arc::clone(&self.reporter));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
            .await
            .unwrap();

        guard.span().message(
            log::Level::Info,
            &format!("listening on {:?}", listener.local_addr()),
        );

        axum::serve(listener, app).await
    }
}

async fn v2(State(reporter): State<Arc<dyn Reporter>>) -> impl IntoResponse {
    Span::record(
        Arc::clone(&reporter),
        "Received GET request for /v2/",
        ScopeKind::Task,
        Outcome::Ok,
    );

    (
        StatusCode::OK,
        [("Docker-Distribution-Api-Version", "registry/2.0")],
        "hello world",
    )
}
