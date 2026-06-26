use crate::digest::Digest;
use std::path::PathBuf;

pub struct BlobPaths {
    pub final_root: PathBuf,
    pub temp_file: PathBuf,
    pub final_file: PathBuf,
    pub mediatype_file: PathBuf,
}

impl BlobPaths {
    pub fn new(blob_root: &PathBuf, digest: &Digest) -> Self {
        let sha = digest.hex().to_string();
        let prefix = sha[0..2].to_string();

        let mut final_root = blob_root.clone();
        final_root.push("sha256");
        final_root.push(prefix);
        final_root.push(&sha);

        let mut temp_file = final_root.clone();
        temp_file.push(format!("{sha}.tmp"));

        let mut final_file = final_root.clone();
        final_file.push(&sha);

        let mut mediatype_file = final_root.clone();
        mediatype_file.push(format!("{sha}.mediatype"));

        Self {
            final_root,
            temp_file,
            final_file,
            mediatype_file,
        }
    }
}
