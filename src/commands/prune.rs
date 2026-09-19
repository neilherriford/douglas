use crate::cli::OutputStyle;
use crate::commands::{CommandContext, print_error};
use bract_client::Client;
use crossterm::style::Stylize;
use std::{io, process::ExitCode};

fn print_deadwood(deadwood: &bract_types::Deadwood) {
    print_deadwood_group("Containers", &deadwood.containers);
    print_deadwood_group("Networks", &deadwood.networks);
    print_deadwood_group("Route files", &deadwood.route_files);
    print_deadwood_group("Resin repositories", &deadwood.resin_repositories);
    print_deadwood_group("Mounts", &deadwood.mounts);
    print_deadwood_group("OpenBao secrets", &deadwood.openbao_secrets);
}

fn print_deadwood_group<T: std::fmt::Display>(label: &str, names: &[T]) {
    if names.is_empty() {
        return;
    }
    println!("{}", label.bold());
    for name in names {
        println!("  - {name}");
    }
}

fn confirm_prune() -> bool {
    print!("Prune the above? [y/N] ");
    if io::Write::flush(&mut io::stdout()).is_err() {
        return false;
    }

    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        return false;
    }

    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

pub(crate) async fn prune_deadwood(output_style: OutputStyle, skip_confirmation: bool) -> ExitCode {
    let context = CommandContext::plain();
    let guard = context.task("Finding deadwood");
    let client = bract_client::UdsClient::new(guard.reporter(), &context.douglas_folders);

    let deadwood = match client.find_deadwood().await {
        Ok(deadwood) => deadwood,
        Err(err) => {
            let message = format!("Could not find deadwood: {err}");
            guard.span().message(log::Level::Warn, &message);
            print_error(output_style, &err.to_string());
            return ExitCode::from(1);
        }
    };

    if deadwood.is_empty() {
        match output_style {
            OutputStyle::Plain => println!("No deadwood found."),
            OutputStyle::Json => println!(
                "{}",
                serde_json::json!({ "deadwood": deadwood, "pruned": false })
            ),
        }
        guard.finish_with_outcome(log::Outcome::Ok);
        return ExitCode::from(0);
    }

    let has_prunable = !deadwood.is_empty();

    if let OutputStyle::Plain = output_style {
        print_deadwood(&deadwood);
    }

    if !has_prunable {
        if let OutputStyle::Json = output_style {
            println!(
                "{}",
                serde_json::json!({ "deadwood": deadwood, "pruned": false })
            );
        }
        guard.finish_with_outcome(log::Outcome::Ok);
        return ExitCode::from(0);
    }

    if !skip_confirmation && !confirm_prune() {
        println!("Not pruning.");
        guard.finish_with_outcome(log::Outcome::Ok);
        return ExitCode::from(0);
    }

    match client.prune_deadwood(&deadwood).await {
        Ok(()) => match output_style {
            OutputStyle::Plain => println!("Pruned."),
            OutputStyle::Json => println!(
                "{}",
                serde_json::json!({ "deadwood": deadwood, "pruned": true })
            ),
        },
        Err(err) => {
            let message = format!("Could not prune deadwood: {err}");
            guard.span().message(log::Level::Warn, &message);
            print_error(output_style, &err.to_string());
            return ExitCode::from(1);
        }
    }

    guard.finish_with_outcome(log::Outcome::Ok);

    ExitCode::from(0)
}
