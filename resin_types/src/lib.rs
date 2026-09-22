mod name;
mod repository;
pub use name::{Name, NameParseError, RepositoryComponent};
pub use repository::{Repository, RepositoryError, RepositoryPath, Upstream};

pub const DEFAULT_PORT: u16 = 7376;
