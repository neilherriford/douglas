use colored::Colorize;
use log::Logger;
use std::fmt::Debug;

#[derive(Debug)]
pub struct TeeLogger {
    logger: Box<dyn Logger>,
}

impl TeeLogger {
    pub fn new(logger: Box<dyn Logger>) -> Self {
        Self { logger }
    }
}

impl Logger for TeeLogger {
    fn debug(&self, message: &str) {
        println!("{}", message.bright_purple());
        self.logger.debug(message);
    }

    fn info(&self, message: &str) {
        println!("{}", message.bright_cyan());
        self.logger.info(message);
    }

    fn warn(&self, message: &str) {
        println!("{}", message.bright_yellow());
        self.logger.warn(message);
    }

    fn error(&self, message: &str) {
        println!("{}", message.bright_red());
        self.logger.error(message);
    }
}
