//! Server-paid typed ref selection. Credentials and connection pools stay on the service.

use std::time::Duration;

use extend_protocol::ErrorCode;
use silicon_extend_client::ref_actions::{Decision, ModelClient, ModelConfig, Observation, Provider, RefError};
use tokio::sync::Semaphore;

use crate::config::JevConfig;
use crate::db::World;
use crate::error::{AppError, AppResult};

pub struct ManagedJev {
    production: Option<ModelClient>,
    testing: Option<ModelClient>,
    in_flight: Semaphore,
}

impl ManagedJev {
    pub fn new(config: &JevConfig) -> Self {
        let client = |key: &Option<String>| {
            key.as_ref().filter(|key| !key.trim().is_empty()).and_then(|key| {
                ModelClient::new(ModelConfig {
                    provider: Provider::Jev,
                    endpoint: config.endpoint.clone(),
                    api_key: key.clone(),
                    model: config.model.clone(),
                    threshold: 0.7,
                    timeout: Duration::from_secs(30),
                })
                // A bad optional provider configuration must not stop ordinary device commands.
                .map_err(|_| tracing::warn!("managed Jev configuration unavailable"))
                .ok()
            })
        };
        Self {
            production: client(&config.api_key),
            testing: client(&config.test_api_key),
            in_flight: Semaphore::new(16),
        }
    }

    pub async fn choose(
        &self,
        world: &World,
        observation: &Observation,
        instruction: &str,
        threshold: f64,
    ) -> AppResult<Decision> {
        // Selection derives from the authenticated request's world, never the caller's body or
        // the deployment mode. Missing test credentials cannot fall back to the production key.
        let client = if world.is_test() {
            &self.testing
        } else {
            &self.production
        };
        let client = client
            .as_ref()
            .ok_or_else(|| unavailable("Managed Jev is not configured for this environment."))?;
        let _permit = self
            .in_flight
            .try_acquire()
            .map_err(|_| unavailable("Managed Jev is busy. Retry later or continue using normal ref commands."))?;
        client
            .choose_with_threshold(observation, instruction, threshold)
            .await
            .map_err(ref_error)
    }
}

pub fn ref_error(error: RefError) -> AppError {
    match error {
        RefError::Invalid(message) => AppError::invalid(message),
        // Do not propagate provider response bodies or transport errors to logs or callers.
        RefError::Provider(_) => unavailable("Managed Jev is unavailable right now."),
    }
}

fn unavailable(message: &str) -> AppError {
    AppError::new(ErrorCode::ServiceUnavailable, message)
        .hint("No action was executed. Use a fresh snapshot and the normal ref commands, or set your own TYPESAFE_API_KEY.")
        .details(serde_json::json!({"action_executed": false, "normal_refs_available": true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_test_configuration_does_not_borrow_the_production_key() {
        let config = JevConfig {
            api_key: Some("server-secret".into()),
            endpoint: "http://127.0.0.1:1".into(),
            ..Default::default()
        };
        assert!(!format!("{config:?}").contains("server-secret"));
        let managed = ManagedJev::new(&config);
        assert!(managed.production.is_some());
        assert!(managed.testing.is_none());
        let observation = Observation::from_snapshot(
            &serde_json::json!({"nodes":[{"ref":"@e1","role":"button","name":"Save"}]}),
            &["click".into()],
            false,
        )
        .unwrap();
        let error = managed
            .choose(&World::test(uuid::Uuid::new_v4()), &observation, "Click Save", 0.7)
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::ServiceUnavailable);
        assert!(error.0.message.contains("not configured"));
    }

    #[test]
    fn invalid_optional_configuration_is_disabled_without_failing_service_start() {
        let managed = ManagedJev::new(&JevConfig {
            api_key: Some("unused".into()),
            endpoint: "http://public.example.invalid/jev".into(),
            ..Default::default()
        });
        assert!(managed.production.is_none());
    }
}
