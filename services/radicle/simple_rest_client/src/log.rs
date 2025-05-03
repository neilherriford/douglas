use colored::Colorize;
use mockall::automock;
use std::fmt::Debug;

#[automock]
pub trait Logger: Send + Sync + Debug {
    fn debug(&self, message: &str);
    fn info(&self, message: &str);
    fn error(&self, message: &str);
}

#[derive(Debug, Default)]
pub struct StdOutLogger {}

impl StdOutLogger {
    pub fn new() -> Self {
        Self {}
    }
}

impl Logger for StdOutLogger {
    fn debug(&self, message: &str) {
        println!("{} {}", "[dbg]".yellow().bold(), message)
    }
    fn info(&self, message: &str) {
        println!("{} {}", "[inf]".cyan().bold(), message)
    }

    fn error(&self, message: &str) {
        println!("{} {}", "[err]".red().bold(), message)
    }
}

#[derive(Debug, Default)]
pub struct SilentLogger {}

impl SilentLogger {
    pub fn new() -> Self {
        Self {}
    }
}

impl Logger for SilentLogger {
    fn debug(&self, _message: &str) {}
    fn info(&self, _message: &str) {}

    fn error(&self, _message: &str) {}
}
