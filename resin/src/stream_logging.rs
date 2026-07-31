use log::{Outcome, Reporter, ScopeKind, Span};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

pub(crate) struct LoggingReader {
    inner: Box<dyn AsyncRead + Send + Unpin>,
    reporter: Arc<dyn Reporter>,
    context: String,
}

impl LoggingReader {
    pub(crate) fn new(
        inner: Box<dyn AsyncRead + Send + Unpin>,
        reporter: Arc<dyn Reporter>,
        context: String,
    ) -> Self {
        Self {
            inner,
            reporter,
            context,
        }
    }
}

impl AsyncRead for LoggingReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);

        if let Poll::Ready(Err(err)) = &result {
            let guard = Span::new(Arc::clone(&this.reporter), "Stream error", ScopeKind::Task)
                .start_guard();
            guard.span().message(
                log::Level::Warn,
                &format!(
                    "{}: streaming failed after response headers were already sent: {err}",
                    this.context
                ),
            );
            guard.finish_with_outcome(Outcome::Failed);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Event;
    use tokio::io::AsyncReadExt;

    struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }

    fn test_reporter() -> Arc<dyn Reporter> {
        Arc::new(NullReporter)
    }

    struct ErroringReader;

    impl AsyncRead for ErroringReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::other("boom")))
        }
    }

    #[tokio::test]
    async fn test_logging_reader_should_pass_bytes_through_unchanged() {
        let inner: Box<dyn AsyncRead + Send + Unpin> =
            Box::new(std::io::Cursor::new(b"hello".to_vec()));
        let mut reader = LoggingReader::new(inner, test_reporter(), "ctx".to_string());

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();

        assert_eq!(buf, b"hello");
    }

    #[tokio::test]
    async fn test_logging_reader_should_propagate_errors_after_logging() {
        let inner: Box<dyn AsyncRead + Send + Unpin> = Box::new(ErroringReader);
        let mut reader = LoggingReader::new(inner, test_reporter(), "ctx".to_string());

        let mut buf = Vec::new();
        let result = reader.read_to_end(&mut buf).await;

        assert!(result.is_err());
    }
}
