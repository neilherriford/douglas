use crate::digest::Digest;
use std::path::{Path, PathBuf};

pub fn create_sharded_blob_path(root: &Path, digest: &Digest) -> PathBuf {
    let mut result = root.to_path_buf();
    result.push("blobs");
    result.push("sha256");
    let hex = digest.hex();
    let prefix = &hex[0..2];

    result.push(prefix);
    result.push(hex);

    result
}
