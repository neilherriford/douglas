use log::{Event, Reporter};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Mutex;

pub struct PipeReporter {
    writer: Mutex<Option<BufWriter<File>>>,
}

impl PipeReporter {
    pub fn new(file: File) -> Self {
        Self {
            writer: Mutex::new(Some(BufWriter::new(file))),
        }
    }
}

impl Reporter for PipeReporter {
    fn emit(&self, event: Event) {
        let mut guard = self.writer.lock().unwrap();
        let Some(writer) = guard.as_mut() else { return };

        let result = serde_json::to_string(&event)
            .map_err(|_| ())
            .and_then(|line| writeln!(writer, "{line}").map_err(|_| ()))
            .and_then(|_| writer.flush().map_err(|_| ()));

        if result.is_err() {
            *guard = None;
        }
    }
}
