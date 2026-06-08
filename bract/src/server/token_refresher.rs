use config::constants::DOUGLAS_ADMIN_GROUP;
use file_system::{FileWriter, Modes, Permissions};
use log::{Level, Outcome, ScopeKind, ScopedReporter, Span};
use os::Os;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

pub(super) struct TokenRefresher {
    token_path: PathBuf,
    permissions: Arc<dyn Permissions>,
    file_writer: Arc<dyn FileWriter>,
    os: Arc<dyn Os>,
}

impl TokenRefresher {
    pub fn new(
        token_path: &Path,
        permissions: Arc<dyn Permissions>,
        file_writer: Arc<dyn FileWriter>,
        os: Arc<dyn Os>,
    ) -> Self {
        Self {
            token_path: token_path.to_path_buf(),
            permissions,
            file_writer,
            os,
        }
    }

    pub fn refresh(&self, span: &Span) {
        let child_span = span.create_child("Refreshing token", ScopeKind::Step);
        let log = child_span.create_scoped_reporter();

        let token = Uuid::now_v7().to_string();
        self.assert_ok(
            &child_span,
            self.file_writer.write_all(&self.token_path.clone(), &token),
        );

        self.assert_ok(
            &child_span,
            self.permissions.change_user_and_group_ownership(
                &self.token_path,
                credentials::ROOT_USER_NAME,
                DOUGLAS_ADMIN_GROUP,
            ),
        );

        self.assert_ok(
            &child_span,
            self.permissions
                .change_mode(&self.token_path, &Modes::OwnerReadWriteGroupRead),
        );

        log.finish(log::Outcome::Ok);
    }

    fn assert_ok<R, E>(&self, span: &Span, result: Result<R, E>)
    where
        E: Debug,
    {
        match result {
            Ok(_) => (),
            Err(err) => {
                let scoped_reporter = span.create_scoped_reporter();
                scoped_reporter.message(
                    Level::Warn,
                    &format!("Token refresh error: {:?}!  Exiting!", err),
                );
                scoped_reporter.finish(Outcome::Failed);
                std::thread::sleep(Duration::from_millis(200));
                self.os.exit(-1);
            }
        }
    }
}
