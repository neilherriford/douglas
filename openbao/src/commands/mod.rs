use serde::Deserialize;
use simple_rest_client::Header;

pub(crate) mod acl_policy;
pub(crate) mod acme;
pub(crate) mod auth;
pub(crate) mod configure_cluster;
pub(crate) mod configure_urls;
pub(crate) mod init;
pub(crate) mod log_in;
pub(crate) mod mounts;
pub(crate) mod pki_role;
pub(crate) mod root_ca;
pub(crate) mod status;
pub(crate) mod unseal;

pub(crate) mod utils {
    pub(crate) mod headers {
        use simple_rest_client::Header;

        pub(crate) fn vault_token(token: &str) -> Header {
            Header::new("X-Vault-Token", token)
        }
    }
}

#[derive(Debug, Deserialize)]
struct AuthWrapper<T> {
    auth: T,
}

#[derive(Debug, Deserialize)]
struct DataWrapper<T> {
    data: T,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KeysData {
    pub(crate) keys: Vec<String>,
}

fn open_bao_token_header(token: &str) -> Header {
    Header::new("X-Vault-Token", token)
}

#[cfg(test)]
pub(crate) mod test_support {
    use log::{Event, Reporter};

    pub(crate) struct NullReporter;

    impl Reporter for NullReporter {
        fn emit(&self, _event: Event) {}
    }
}
