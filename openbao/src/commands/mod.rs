pub(crate) mod auth;
pub(crate) mod init;
pub(crate) mod status;
pub(crate) mod unseal;
pub(crate) mod upsert_acl_policy;

pub(crate) mod utils {
    pub(crate) mod headers {
        use simple_rest_client::Header;

        pub(crate) fn valut_token(token: &str) -> Header {
            Header::new("X-Vault-Token", token)
        }
    }
}
