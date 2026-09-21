use crate::blueprints::IgnoreMissing;
use async_trait::async_trait;
use blueprint::Command;
use docker::client::ContainerRef;
use docker_types::ContainerName;
use log::{ScopeKind, Span};

pub(crate) trait HasDockerClient {
    fn docker_client(&self) -> &dyn docker::client::Client;
}

pub(crate) struct Subject {
    noun: &'static str,
    label: String,
}

impl Subject {
    pub(crate) fn seedling(
        name: &seedbank_types::Name,
        version: Option<seedbank_types::Version>,
    ) -> Self {
        Self::new("seedling", name.as_ref(), version)
    }

    pub(crate) fn container(name: &ContainerName, version: seedbank_types::Version) -> Self {
        Self::new("container", name.as_ref(), Some(version))
    }

    fn new(noun: &'static str, name: &str, version: Option<seedbank_types::Version>) -> Self {
        let label = match version {
            Some(version) => format!("'{name}' (v{version})"),
            None => format!("'{name}'"),
        };
        Self { noun, label }
    }

    fn describe(&self) -> String {
        format!("{} {}", self.noun, self.label)
    }
}

pub(crate) struct StopContainerStep {
    subject: Subject,
    container: ContainerName,
}

impl StopContainerStep {
    pub(crate) fn new(subject: Subject, container: ContainerName) -> Self {
        Self { subject, container }
    }
}

impl std::fmt::Display for StopContainerStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Stopping {}", self.subject.describe())
    }
}

#[async_trait]
impl<C: HasDockerClient + Send> Command<C> for StopContainerStep {
    fn name(&self) -> String {
        format!("Stopping {}", self.subject.noun)
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Stopping {}…", self.subject.describe()),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .docker_client()
            .stop_container(ContainerRef::FullName(self.container.clone()))
            .await?;

        guard.finish(Ok(()))
    }

    async fn rollback(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Restarting {}", self.subject.describe()),
                ScopeKind::Step,
            )
            .start_guard();

        let result = context
            .docker_client()
            .start_container(ContainerRef::FullName(self.container.clone()))
            .await
            .ignore_missing();

        guard.finish(result).map_err(Into::into)
    }
}

pub(crate) struct DropContainerStep {
    subject: Subject,
    container: ContainerName,
}

impl DropContainerStep {
    pub(crate) fn new(subject: Subject, container: ContainerName) -> Self {
        Self { subject, container }
    }
}

impl std::fmt::Display for DropContainerStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Dropping {}", self.subject.describe())
    }
}

#[async_trait]
impl<C: HasDockerClient + Send> Command<C> for DropContainerStep {
    fn name(&self) -> String {
        format!("Dropping {}", self.subject.noun)
    }

    async fn run(
        &mut self,
        span: &Span,
        context: &mut C,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let guard = span
            .create_child(
                &format!("Dropping {}…", self.subject.describe()),
                ScopeKind::Step,
            )
            .start_guard();

        context
            .docker_client()
            .delete_container(ContainerRef::FullName(self.container.clone()))
            .await?;

        guard.finish(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bract_types::container_name;
    use docker::DockerError;

    struct TestContext<'a> {
        docker_client: &'a dyn docker::client::Client,
    }

    impl HasDockerClient for TestContext<'_> {
        fn docker_client(&self) -> &dyn docker::client::Client {
            self.docker_client
        }
    }

    struct NullReporter;

    impl log::Reporter for NullReporter {
        fn emit(&self, _event: log::Event) {}
    }

    fn span() -> Span {
        Span::new(std::sync::Arc::new(NullReporter), "test", ScopeKind::Group)
    }

    fn seedling() -> seedbank_types::Name {
        "traefik".parse().unwrap()
    }

    fn container() -> ContainerName {
        container_name(&seedling()).unwrap()
    }

    #[test]
    fn test_a_seedling_step_should_name_the_seedling_and_its_version() {
        let step = StopContainerStep::new(
            Subject::seedling(&seedling(), Some(seedbank_types::Version(1))),
            container(),
        );

        assert_eq!(step.to_string(), "Stopping seedling 'traefik' (v1)");
    }

    #[test]
    fn test_a_seedling_step_should_leave_out_a_version_it_does_not_have() {
        let step = DropContainerStep::new(Subject::seedling(&seedling(), None), container());

        assert_eq!(step.to_string(), "Dropping seedling 'traefik'");
    }

    #[test]
    fn test_a_container_step_should_name_the_container_and_its_version() {
        let step = StopContainerStep::new(
            Subject::container(&container(), seedbank_types::Version(2)),
            container(),
        );

        assert_eq!(step.to_string(), "Stopping container 'doug.traefik' (v2)");
    }

    #[test]
    fn test_the_step_names_should_use_the_noun_only() {
        let seedling_step =
            StopContainerStep::new(Subject::seedling(&seedling(), None), container());
        let container_step = DropContainerStep::new(
            Subject::container(&container(), seedbank_types::Version(1)),
            container(),
        );

        assert_eq!(
            <StopContainerStep as Command<TestContext<'_>>>::name(&seedling_step),
            "Stopping seedling"
        );
        assert_eq!(
            <DropContainerStep as Command<TestContext<'_>>>::name(&container_step),
            "Dropping container"
        );
    }

    #[tokio::test]
    async fn test_stop_should_stop_the_container() {
        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_stop_container()
            .withf(|container_ref| matches!(container_ref, ContainerRef::FullName(name) if *name == container()))
            .times(1)
            .returning(|_| Ok(()));
        let mut context = TestContext {
            docker_client: &docker_client,
        };
        let mut step = StopContainerStep::new(Subject::seedling(&seedling(), None), container());

        let result = step.run(&span(), &mut context).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stop_should_fail_when_docker_cannot_stop_the_container() {
        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_stop_container()
            .returning(|_| Err(DockerError::PingFailed("down".to_string())));
        let mut context = TestContext {
            docker_client: &docker_client,
        };
        let mut step = StopContainerStep::new(Subject::seedling(&seedling(), None), container());

        let result = step.run(&span(), &mut context).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_stop_rollback_should_start_the_container_again() {
        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_start_container()
            .times(1)
            .returning(|_| Ok(()));
        let mut context = TestContext {
            docker_client: &docker_client,
        };
        let mut step = StopContainerStep::new(Subject::seedling(&seedling(), None), container());

        let result = step.rollback(&span(), &mut context).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stop_rollback_should_not_mind_a_container_that_is_already_gone() {
        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_start_container()
            .returning(|_| Err(DockerError::ResourceNotFound));
        let mut context = TestContext {
            docker_client: &docker_client,
        };
        let mut step = StopContainerStep::new(Subject::seedling(&seedling(), None), container());

        let result = step.rollback(&span(), &mut context).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stop_rollback_should_fail_on_any_other_docker_error() {
        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_start_container()
            .returning(|_| Err(DockerError::PingFailed("down".to_string())));
        let mut context = TestContext {
            docker_client: &docker_client,
        };
        let mut step = StopContainerStep::new(Subject::seedling(&seedling(), None), container());

        let result = step.rollback(&span(), &mut context).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_drop_should_delete_the_container() {
        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_delete_container()
            .times(1)
            .returning(|_| Ok(()));
        let mut context = TestContext {
            docker_client: &docker_client,
        };
        let mut step = DropContainerStep::new(Subject::seedling(&seedling(), None), container());

        let result = step.run(&span(), &mut context).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_drop_should_fail_when_docker_cannot_delete_the_container() {
        let mut docker_client = docker::MockClient::new();
        docker_client
            .expect_delete_container()
            .returning(|_| Err(DockerError::PingFailed("down".to_string())));
        let mut context = TestContext {
            docker_client: &docker_client,
        };
        let mut step = DropContainerStep::new(Subject::seedling(&seedling(), None), container());

        let result = step.run(&span(), &mut context).await;

        assert!(result.is_err());
    }
}
