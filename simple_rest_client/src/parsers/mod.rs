pub mod json;

pub trait Parser<T>: Send + Sync + std::fmt::Debug {
    type ParseError: std::error::Error + Send + Sync + 'static;
    fn parse(&self, input: String) -> Result<T, Self::ParseError>;
}
