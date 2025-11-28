use mockall::automock;

#[macro_export]
macro_rules! unwrap_or_log {
    ($logger:expr => $method:ident, $result:expr) => {{
        match $result {
            Ok(val) => val,
            Err(err) => {
                $logger.$method(&err.to_string());
                return Err(err);
            }
        }
    }};
}

#[automock]
pub trait ClientErrorDisplay {
    fn to_client_string(&self) -> String;
}
