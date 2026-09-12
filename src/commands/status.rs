use crate::cli::OutputStyle;
use crate::daemon::build_plain_reporter;
use ::config::DouglasFolders;
use bract_client::Client;
use file_system::{Folder, UnixFileReader, UnixFolder};
use log::Span;
use resin_client::ClientBuilder as _;
use std::{process::ExitCode, sync::Arc};

#[derive(serde::Serialize)]
struct SeedlingStatusEntry {
    name: String,
    status: String,
}

#[derive(serde::Serialize)]
struct CoreServiceStatus {
    name: String,
    running: bool,
    detail: String,
    supervisor_gave_up: Option<woodward::SupervisionFailure>,
}

#[derive(serde::Serialize)]
struct StatusReport {
    seedlings: Vec<SeedlingStatusEntry>,
    seedlings_error: Option<String>,
    core_services: Vec<CoreServiceStatus>,
    traefik_routes: Vec<String>,
    traefik_routes_error: Option<String>,
    openbao: Option<bract_types::OpenBaoReport>,
    openbao_error: Option<String>,
}

fn bract_error_means_unreachable(err: &bract_client::Error) -> bool {
    matches!(
        err,
        bract_client::Error::MissingSocket
            | bract_client::Error::ConnectionRefused
            | bract_client::Error::NoResponse
            | bract_client::Error::IoError(_)
    )
}

fn probe_bract(
    seedling_names_result: &Result<Vec<seedbank_types::Name>, bract_client::Error>,
) -> CoreServiceStatus {
    let running = !matches!(seedling_names_result, Err(err) if bract_error_means_unreachable(err));
    let detail = match seedling_names_result {
        Ok(names) => format!("{} seedling(s) registered", names.len()),
        Err(err) => err.to_string(),
    };
    CoreServiceStatus {
        name: "bract".to_string(),
        running,
        detail,
        supervisor_gave_up: None,
    }
}

fn probe_seedbank(
    seedling_names_result: &Result<Vec<seedbank_types::Name>, bract_client::Error>,
) -> CoreServiceStatus {
    let (running, detail) = match seedling_names_result {
        Ok(names) => (true, format!("{} seedling(s) registered", names.len())),
        Err(err) if bract_error_means_unreachable(err) => (
            false,
            "bract is unreachable, cannot determine seedbank status".to_string(),
        ),
        Err(err) => (false, err.to_string()),
    };
    CoreServiceStatus {
        name: "seedbank".to_string(),
        running,
        detail,
        supervisor_gave_up: None,
    }
}

async fn probe_resin(reporter: Arc<dyn log::Reporter>) -> CoreServiceStatus {
    let (running, detail) = match resin_client::LocalhostClientBuilder.build(reporter).await {
        Ok(mut client) => match client.list_repositories().await {
            Ok(repositories) => (true, format!("{} repositories", repositories.len())),
            Err(err) => (false, err.to_string()),
        },
        Err(err) => (false, err.to_string()),
    };
    CoreServiceStatus {
        name: "resin".to_string(),
        running,
        detail,
        supervisor_gave_up: None,
    }
}

fn probe_woodward(douglas_folders: &DouglasFolders, span: &Span) -> CoreServiceStatus {
    let (running, detail) = match woodward::service_definition(douglas_folders).liveness {
        Some(liveness) => match blueprint::listener::check_liveness(span, &liveness) {
            blueprint::RunningStatus::Running => (true, "heartbeat is current".to_string()),
            blueprint::RunningStatus::NotRunning => {
                (false, "heartbeat is stale or missing".to_string())
            }
            blueprint::RunningStatus::Unknown => {
                (false, "could not determine heartbeat freshness".to_string())
            }
        },
        None => (false, "no liveness check configured".to_string()),
    };

    CoreServiceStatus {
        name: config::services::WOODWARD.to_string(),
        running,
        detail,
        supervisor_gave_up: None,
    }
}

fn list_traefik_routes(
    folder: &dyn Folder,
    douglas_folders: &DouglasFolders,
) -> Result<Vec<String>, String> {
    let dynamic_dir = bract::traefik_dynamic_dir(douglas_folders).map_err(|err| err.to_string())?;

    if !folder.exists(&dynamic_dir) {
        return Ok(Vec::new());
    }

    folder
        .entries(&dynamic_dir)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.name.strip_suffix(".yml"))
                .map(std::string::ToString::to_string)
                .collect()
        })
        .map_err(|err| err.to_string())
}

pub(crate) async fn status(output_style: OutputStyle) -> ExitCode {
    let douglas_folders = DouglasFolders::new();
    let reporter = build_plain_reporter(&douglas_folders, "douglas-cli");
    let guard = Span::new(Arc::clone(&reporter), "Status", log::ScopeKind::Task).start_guard();

    let bract_client = bract_client::UdsClient::new(Arc::clone(&reporter), &douglas_folders);

    let seedling_names_result = bract_client.list_seedlings().await;
    let (seedlings, seedlings_error) = match &seedling_names_result {
        Ok(names) => {
            let mut entries = Vec::with_capacity(names.len());
            for name in names {
                let status = match bract_client.seedling_status(name).await {
                    Ok(status) => status.to_string(),
                    Err(err) => format!("unavailable ({err})"),
                };
                entries.push(SeedlingStatusEntry {
                    name: name.to_string(),
                    status,
                });
            }
            (entries, None)
        }
        Err(err) => (Vec::new(), Some(err.to_string())),
    };

    let mut core_services = vec![
        probe_bract(&seedling_names_result),
        probe_resin(Arc::clone(&reporter)).await,
        probe_seedbank(&seedling_names_result),
        probe_woodward(&douglas_folders, guard.span()),
    ];

    let supervisor_reader = UnixFileReader::new();
    for service in &mut core_services {
        service.supervisor_gave_up = woodward::read_supervision_failure(
            &supervisor_reader,
            &douglas_folders.supervisor_failure_marker(&service.name),
        )
        .unwrap_or(None);
    }

    let folder: Arc<dyn Folder> = Arc::new(UnixFolder::new());
    let (traefik_routes, traefik_routes_error) =
        match list_traefik_routes(folder.as_ref(), &douglas_folders) {
            Ok(routes) => (routes, None),
            Err(err) => (Vec::new(), Some(err)),
        };

    let (openbao_report, openbao_error) = match bract_client.openbao_status().await {
        Ok(report) => (Some(report), None),
        Err(err) => (None, Some(err.to_string())),
    };

    print_status_report(
        output_style,
        &StatusReport {
            seedlings,
            seedlings_error,
            core_services,
            traefik_routes,
            traefik_routes_error,
            openbao: openbao_report,
            openbao_error,
        },
    );

    guard.finish_with_outcome(log::Outcome::Ok);
    ExitCode::from(0)
}

fn print_status_report(output_style: OutputStyle, report: &StatusReport) {
    match output_style {
        OutputStyle::Json => {
            if let Ok(json) = serde_json::to_string(report) {
                println!("{json}");
            }
        }
        OutputStyle::Plain => {
            println!("Core services:");
            for service in &report.core_services {
                let state = if service.running {
                    "running"
                } else {
                    "unavailable"
                };
                println!("  {}: {} ({})", service.name, state, service.detail);
                if let Some(failure) = &service.supervisor_gave_up {
                    println!(
                        "    supervisor gave up after {} restarts and {} failed kicks",
                        failure.restart_count, failure.kick_failures
                    );
                }
            }

            println!("Seedlings:");
            if let Some(err) = &report.seedlings_error {
                println!("  unavailable ({err})");
            } else if report.seedlings.is_empty() {
                println!("  none");
            } else {
                for entry in &report.seedlings {
                    println!("  {}: {}", entry.name, entry.status);
                }
            }

            println!("Traefik:");
            if let Some(err) = &report.traefik_routes_error {
                println!("  routes unavailable ({err})");
            } else if report.traefik_routes.is_empty() {
                println!("  routes: none");
            } else {
                println!("  routes:");
                for route in &report.traefik_routes {
                    println!("    {route}");
                }
            }

            println!("OpenBao:");
            match (&report.openbao, &report.openbao_error) {
                (Some(openbao), _) => print_openbao_report_plain(openbao),
                (None, Some(err)) => println!("  unavailable ({err})"),
                (None, None) => println!("  unavailable"),
            }
        }
    }
}

fn print_openbao_report_plain(report: &bract_types::OpenBaoReport) {
    println!("  running: {}", report.is_running);
    if !report.is_running {
        return;
    }
    println!("  initialized: {}", report.is_initialized);
    println!("  sealed: {}", report.is_sealed);
    println!("  credentials available: {}", report.credentials_available);
    println!("  credentials work: {}", report.credentials_work);
    if !report.credentials_work {
        return;
    }
    println!("  approle enabled: {}", report.app_role_enabled);
    println!("  acme enabled: {}", report.acme_enabled);
    println!("  root ca configured: {}", report.root_ca_configured);
    println!("  acme pki role created: {}", report.acme_pki_role_created);
    println!("  mounts:");
    if report.mounts.is_empty() {
        println!("    none");
    } else {
        for (path, kind) in &report.mounts {
            println!("    {path} ({kind})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use file_system::{Entry, EntryKind, FileSystemError, MockFolder};

    fn seedling_name(value: &str) -> seedbank_types::Name {
        let Ok(name) = value.parse() else {
            panic!("'{value}' should be a valid seedling name");
        };
        name
    }

    #[test]
    fn test_bract_error_means_unreachable_should_be_true_for_missing_socket() {
        assert!(bract_error_means_unreachable(
            &bract_client::Error::MissingSocket
        ));
    }

    #[test]
    fn test_bract_error_means_unreachable_should_be_true_for_connection_refused() {
        assert!(bract_error_means_unreachable(
            &bract_client::Error::ConnectionRefused
        ));
    }

    #[test]
    fn test_bract_error_means_unreachable_should_be_false_for_a_server_error() {
        assert!(!bract_error_means_unreachable(
            &bract_client::Error::ServerError("boom".to_string())
        ));
    }

    #[test]
    fn test_probe_bract_should_report_running_with_a_seedling_count_when_bract_responds() {
        let result = Ok(vec![seedling_name("hello-world"), seedling_name("other")]);

        let status = probe_bract(&result);

        assert_eq!(status.name, "bract");
        assert!(status.running);
        assert_eq!(status.detail, "2 seedling(s) registered");
        assert!(status.supervisor_gave_up.is_none());
    }

    #[test]
    fn test_probe_bract_should_report_not_running_when_unreachable() {
        let result = Err(bract_client::Error::ConnectionRefused);

        let status = probe_bract(&result);

        assert!(!status.running);
    }

    #[test]
    fn test_probe_bract_should_still_report_running_on_a_non_unreachable_error() {
        let result = Err(bract_client::Error::ServerError("boom".to_string()));

        let status = probe_bract(&result);

        assert!(status.running);
        assert_eq!(status.detail, "Server error: boom");
    }

    #[test]
    fn test_probe_seedbank_should_report_running_when_bract_reports_seedlings() {
        let result = Ok(vec![seedling_name("hello-world")]);

        let status = probe_seedbank(&result);

        assert_eq!(status.name, "seedbank");
        assert!(status.running);
        assert_eq!(status.detail, "1 seedling(s) registered");
    }

    #[test]
    fn test_probe_seedbank_should_blame_bract_when_bract_itself_is_unreachable() {
        let result = Err(bract_client::Error::MissingSocket);

        let status = probe_seedbank(&result);

        assert!(!status.running);
        assert_eq!(
            status.detail,
            "bract is unreachable, cannot determine seedbank status"
        );
    }

    #[test]
    fn test_probe_seedbank_should_report_not_running_on_a_non_unreachable_error() {
        let result = Err(bract_client::Error::ServerError("boom".to_string()));

        let status = probe_seedbank(&result);

        assert!(!status.running);
        assert_eq!(status.detail, "Server error: boom");
    }

    fn yml_entry(name: &str, dir: &std::path::Path) -> Entry {
        Entry {
            name: name.to_string(),
            path: dir.join(name),
            kind: EntryKind::File,
            is_link: false,
            size: 0,
        }
    }

    #[test]
    fn test_list_traefik_routes_should_be_empty_when_the_dynamic_dir_is_missing() {
        let mut folder = MockFolder::new();
        folder.expect_exists().returning(|_| false);

        let result = list_traefik_routes(&folder, &DouglasFolders::new());

        assert_eq!(result, Ok(Vec::new()));
    }

    #[test]
    fn test_list_traefik_routes_should_strip_the_yml_suffix_from_each_entry() {
        let mut folder = MockFolder::new();
        folder.expect_exists().returning(|_| true);
        folder.expect_entries().returning(|dir| {
            Ok(vec![
                yml_entry("hello-world.yml", dir),
                yml_entry("second-app.yml", dir),
            ])
        });

        let Ok(mut result) = list_traefik_routes(&folder, &DouglasFolders::new()) else {
            panic!("should list routes");
        };
        result.sort();

        assert_eq!(
            result,
            vec!["hello-world".to_string(), "second-app".to_string()]
        );
    }

    #[test]
    fn test_list_traefik_routes_should_propagate_a_read_error() {
        let mut folder = MockFolder::new();
        folder.expect_exists().returning(|_| true);
        folder
            .expect_entries()
            .returning(|dir| Err(FileSystemError::NotFoundError(dir.to_path_buf())));

        let result = list_traefik_routes(&folder, &DouglasFolders::new());

        assert!(result.is_err());
    }
}
