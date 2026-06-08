use log::{Event, Reporter};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::fd::FromRawFd;
use std::sync::Mutex;

pub struct PipeReporter {
    writer: Mutex<BufWriter<File>>,
}

impl PipeReporter {
    pub unsafe fn from_raw_fd(fd: i32) -> Self {
        let file = unsafe { File::from_raw_fd(fd) };
        Self {
            writer: Mutex::new(BufWriter::new(file)),
        }
    }
}

impl Reporter for PipeReporter {
    fn emit(&self, event: Event) {
        let Ok(mut w) = self.writer.lock() else {
            return;
        };
        if let Ok(line) = serde_json::to_string(&event) {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    }
}
