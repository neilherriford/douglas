use hyper::rt::{Read, Write};
use std::error::Error;
use std::future::Future;
use std::pin::Pin;

pub trait IoStream: Read + Write + Unpin + Send {}

impl<T: Read + Write + Unpin + Send> IoStream for T {}

pub trait IoBuilder {
    fn build<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Box<dyn IoStream>, Box<dyn Error + Send + Sync>>>
                + Send
                + 'a,
        >,
    >;
}
