use crate::{io_builder::IoStream, log::Logger};
use hyper::client::conn::http1::SendRequest;
use std::error::Error;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait HttpExecutorBuilder {
    async fn build(
        &self,
        logger: Arc<dyn Logger>,
        io_stream: Box<dyn IoStream + Send + 'static>,
    ) -> Result<Box<dyn HttpExecutor>, Box<dyn Error>>;
}

pub struct HttpClientBuilder {}
impl HttpClientBuilder {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait::async_trait]
impl HttpExecutorBuilder for HttpClientBuilder {
    async fn build(
        &self,
        logger: Arc<dyn Logger>,
        io_stream: Box<dyn IoStream + Send + 'static>,
    ) -> Result<Box<dyn HttpExecutor>, Box<dyn Error>> {
        let (sender, conn) = hyper::client::conn::http1::handshake(io_stream).await?;
        let task_logger = Arc::clone(&logger);

        tokio::spawn(async move {
            if let Err(e) = conn.await {
                task_logger.error(&format!("Connection error: {:?}", e));
            }
        });

        let client_logger = Arc::clone(&logger);
        Ok(Box::new(HttpClient {
            sender: Box::new(StringRequestSender::new(sender)),
            logger: client_logger,
        }))
    }
}

#[async_trait::async_trait]
pub trait HttpExecutor: std::fmt::Debug {
    async fn execute(
        &mut self,
        req: hyper::Request<String>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, Box<dyn Error>>;
}

#[async_trait::async_trait]
trait RequestSender: std::fmt::Debug {
    async fn send(
        &mut self,
        req: hyper::Request<String>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, Box<dyn Error>>;
}

#[derive(Debug)]
struct StringRequestSender {
    sender: SendRequest<String>,
}

impl StringRequestSender {
    pub fn new(sender: SendRequest<String>) -> Self {
        Self { sender }
    }
}

#[async_trait::async_trait]
impl RequestSender for StringRequestSender {
    async fn send(
        &mut self,
        req: hyper::Request<String>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, Box<dyn Error>> {
        match self.sender.send_request(req).await {
            Ok(res) => Ok(res),
            Err(err) => Err(Box::new(err)),
        }
    }
}

#[derive(Debug)]
pub(crate) struct HttpClient {
    sender: Box<dyn RequestSender + Send>,
    logger: Arc<dyn Logger>,
}

#[async_trait::async_trait]
impl HttpExecutor for HttpClient {
    async fn execute(
        &mut self,
        req: hyper::Request<String>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, Box<dyn Error>> {
        let res = self.sender.send_request(req).await?;
        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::MockLogger;
    use core::task::{Context, Poll};
    use hyper::rt::ReadBufCursor;
    use hyper::rt::{Read, Write};
    use std::io::{Error, ErrorKind};
    use std::pin::Pin;
    use std::sync::Arc;

    mod http_executor_builder {
        use super::*;

        struct MockStream {
            create_poll_read_result: Box<
                dyn FnMut(&mut Context<'_>, ReadBufCursor<'_>) -> Poll<Result<(), Error>> + Send,
            >,
            create_poll_write_result:
                Box<dyn FnMut(&mut Context<'_>, &[u8]) -> Poll<Result<usize, Error>> + Send>,
            create_poll_flush_result: Box<dyn Fn() -> Poll<Result<(), Error>> + Send>,
            create_poll_shutdown_result: Box<dyn Fn() -> Poll<Result<(), Error>> + Send>,
        }

        impl Read for MockStream {
            #[allow(unused_variables)]
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: ReadBufCursor<'_>,
            ) -> Poll<Result<(), Error>> {
                (self.create_poll_read_result)(cx, buf)
            }
        }

        impl Write for MockStream {
            #[allow(unused_variables)]
            fn poll_write(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<Result<usize, Error>> {
                (self.create_poll_write_result)(cx, buf)
            }

            #[allow(unused_variables)]
            fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
                (self.create_poll_flush_result)()
            }

            #[allow(unused_variables)]
            fn poll_shutdown(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Result<(), Error>> {
                (self.create_poll_shutdown_result)()
            }
        }

        impl Unpin for MockStream {}

        #[tokio::test(flavor = "current_thread")]
        async fn test_builder_does_not_log_errors_on_successfull_handshake() {
            let mut mock = MockLogger::new();
            mock.expect_error().never();
            let logger: Arc<dyn Logger> = Arc::new(mock);
            let builder = HttpClientBuilder::new();

            let stream = Box::new(MockStream {
                create_poll_read_result: Box::new(move |_cx, _buf| Poll::Pending),
                create_poll_write_result: Box::new(move |_cx, buf| Poll::Ready(Ok(buf.len()))),
                create_poll_flush_result: Box::new(|| Poll::Ready(Ok(()))),
                create_poll_shutdown_result: Box::new(|| Poll::Ready(Ok(()))),
            });
            let result = builder.build(logger, stream).await;
            assert!(result.is_ok());

            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn test_builder_logs_errors_on_failed_handshake() {
            let mut mock = MockLogger::new();
            mock.expect_error()
                .withf(|msg| msg.contains("Connection error") && msg.contains("oops"))
                .times(1)
                .return_const(());

            let logger: Arc<dyn Logger> = Arc::new(mock);
            let builder = HttpClientBuilder::new();

            let stream = Box::new(MockStream {
                create_poll_read_result: Box::new(move |_cx, _buf| {
                    Poll::Ready(Err(Error::new(ErrorKind::Other, "oops")))
                }),
                create_poll_write_result: Box::new(move |_cx, buf| Poll::Ready(Ok(buf.len()))),
                create_poll_flush_result: Box::new(|| Poll::Ready(Ok(()))),
                create_poll_shutdown_result: Box::new(|| Poll::Ready(Ok(()))),
            });
            let result = builder.build(logger, stream).await;
            assert!(result.is_ok());

            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
