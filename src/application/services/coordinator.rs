use anyhow::{Error, Result};
use async_trait::async_trait;
use tokio::task::JoinSet;

use super::ServiceGraphPlan;

#[async_trait]
pub(crate) trait ServicePhaseRuntime: Clone + Send + Sync + 'static {
    async fn start_service(&self, service_name: &str) -> Result<()>;
    async fn await_readiness(&self, service_name: String) -> Result<()>;
}

pub(crate) struct ServicePhaseCoordinator<'a> {
    graph: &'a ServiceGraphPlan,
}

impl<'a> ServicePhaseCoordinator<'a> {
    pub(crate) fn new(graph: &'a ServiceGraphPlan) -> Self {
        Self { graph }
    }

    pub(crate) async fn run<R>(&self, runtime: R) -> Result<()>
    where
        R: ServicePhaseRuntime,
    {
        for layer in self.graph.layers() {
            for service_name in layer {
                runtime.start_service(service_name).await?;
            }

            let mut tasks = JoinSet::new();
            for service_name in layer {
                let runtime = runtime.clone();
                let service_name = service_name.clone();
                tasks.spawn(async move { runtime.await_readiness(service_name).await });
            }

            while let Some(result) = tasks.join_next().await {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        tasks.abort_all();
                        while tasks.join_next().await.is_some() {}
                        return Err(err);
                    }
                    Err(err) => {
                        tasks.abort_all();
                        while tasks.join_next().await.is_some() {}
                        return Err(Error::new(err));
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use capsule_core::types::ServiceSpec;

    use super::{ServicePhaseCoordinator, ServicePhaseRuntime};
    use crate::application::services::ServiceGraphPlan;

    fn service(entrypoint: &str, depends_on: Option<Vec<&str>>) -> ServiceSpec {
        ServiceSpec {
            entrypoint: entrypoint.to_string(),
            target: None,
            depends_on: depends_on
                .map(|deps| deps.into_iter().map(|value| value.to_string()).collect()),
            expose: None,
            env: None,
            state_bindings: Vec::new(),
            readiness_probe: None,
            network: None,
        }
    }

    #[derive(Clone, Default)]
    struct RecorderRuntime {
        events: Arc<Mutex<Vec<String>>>,
        fail_on_ready: Option<String>,
    }

    #[async_trait]
    impl ServicePhaseRuntime for RecorderRuntime {
        async fn start_service(&self, service_name: &str) -> Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("start:{service_name}"));
            Ok(())
        }

        async fn await_readiness(&self, service_name: String) -> Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("ready:{service_name}"));
            if self.fail_on_ready.as_deref() == Some(service_name.as_str()) {
                return Err(anyhow!(format!("{service_name} failed readiness")));
            }
            Ok(())
        }
    }

    fn position(events: &[String], needle: &str) -> usize {
        events.iter().position(|value| value == needle).unwrap()
    }

    #[tokio::test]
    async fn service_phase_coordinator_applies_layer_barriers() {
        let mut services = HashMap::new();
        services.insert(
            "main".to_string(),
            service("node main.js", Some(vec!["api"])),
        );
        services.insert(
            "api".to_string(),
            service("node api.js", Some(vec!["cache", "db"])),
        );
        services.insert("cache".to_string(), service("node cache.js", None));
        services.insert("db".to_string(), service("node db.js", None));

        let graph = ServiceGraphPlan::from_services(&services).unwrap();
        let coordinator = ServicePhaseCoordinator::new(&graph);
        let runtime = RecorderRuntime::default();

        coordinator.run(runtime.clone()).await.unwrap();

        let events = runtime.events.lock().unwrap().clone();
        let start_cache = position(&events, "start:cache");
        let start_db = position(&events, "start:db");
        let ready_cache = position(&events, "ready:cache");
        let ready_db = position(&events, "ready:db");
        let start_api = position(&events, "start:api");
        let ready_api = position(&events, "ready:api");
        let start_main = position(&events, "start:main");
        let ready_main = position(&events, "ready:main");

        assert!(start_cache < ready_cache);
        assert!(start_db < ready_db);
        assert!(ready_cache < start_api);
        assert!(ready_db < start_api);
        assert!(start_api < ready_api);
        assert!(ready_api < start_main);
        assert!(start_main < ready_main);
    }

    #[tokio::test]
    async fn service_phase_coordinator_stops_after_layer_failure() {
        let mut services = HashMap::new();
        services.insert(
            "main".to_string(),
            service("node main.js", Some(vec!["api"])),
        );
        services.insert("api".to_string(), service("node api.js", Some(vec!["db"])));
        services.insert("db".to_string(), service("node db.js", None));

        let graph = ServiceGraphPlan::from_services(&services).unwrap();
        let coordinator = ServicePhaseCoordinator::new(&graph);
        let runtime = RecorderRuntime {
            fail_on_ready: Some("db".to_string()),
            ..RecorderRuntime::default()
        };

        let err = coordinator.run(runtime.clone()).await.unwrap_err();
        let events = runtime.events.lock().unwrap().clone();

        assert!(err.to_string().contains("db failed readiness"));
        assert_eq!(events, vec!["start:db", "ready:db"]);
    }
}
