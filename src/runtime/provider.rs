use std::time::Duration;

use async_openai::{
    Client,
    config::OpenAIConfig,
    types::responses::{
        CreateResponseArgs, ResponseFormatJsonSchema, ResponseTextParam,
        TextResponseFormatConfiguration,
    },
};
use serde::Serialize;
use serde_json::json;

use super::agent_worker::{
    AgentContext, AgentOutcome, AgentSession, AgentWorker, AgentWorkerError,
};
use crate::store::RuntimeTaskLease;

const DEFAULT_INSTRUCTIONS: &str = r#"
You are a worker inside a durable agent runtime.
Return exactly one JSON object describing the next durable outcome.
Do not claim that an external side effect happened unless the task input proves it.
Use "completed" only when the task is finished, "checkpointed" when meaningful
progress was made but more work remains, "failed" for a terminal failure,
"proposed_plan" when a plan change must be approved, and "waiting_for_approval"
when execution must stop until a human decision.
"#;

#[derive(Debug, Serialize)]
struct AgentTaskRequest<'a> {
    schema_version: u8,
    context: &'a AgentContext,
    task_kind: &'a str,
    payload: &'a serde_json::Value,
}

#[derive(Debug)]
pub struct OpenAiAgentWorker {
    client: Client<OpenAIConfig>,
    model: String,
    instructions: String,
    timeout: Duration,
    runtime: tokio::runtime::Runtime,
}

impl OpenAiAgentWorker {
    pub fn from_env(
        model: impl Into<String>,
        timeout_seconds: u64,
    ) -> Result<Self, AgentWorkerError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| AgentWorkerError::Provider {
                message: "OPENAI_API_KEY is not configured".to_owned(),
            })?;
        Self::with_base_url(
            api_key,
            model,
            timeout_seconds,
            std::env::var("OPENAI_BASE_URL").ok(),
        )
    }

    pub fn with_options(
        api_key: String,
        model: impl Into<String>,
        timeout_seconds: u64,
    ) -> Result<Self, AgentWorkerError> {
        Self::with_base_url(api_key, model, timeout_seconds, None)
    }

    pub fn with_base_url(
        api_key: String,
        model: impl Into<String>,
        timeout_seconds: u64,
        base_url: Option<String>,
    ) -> Result<Self, AgentWorkerError> {
        if api_key.trim().is_empty() {
            return Err(AgentWorkerError::Provider {
                message: "OpenAI API key must not be empty".to_owned(),
            });
        }
        if timeout_seconds == 0 {
            return Err(AgentWorkerError::Provider {
                message: "provider timeout must be greater than zero".to_owned(),
            });
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| AgentWorkerError::Provider {
                message: format!("failed to create provider runtime: {error}"),
            })?;
        let mut config = OpenAIConfig::new().with_api_key(api_key);
        if let Some(base_url) = base_url.filter(|value| !value.trim().is_empty()) {
            config = config.with_api_base(base_url);
        }
        Ok(Self {
            client: Client::with_config(config),
            model: model.into(),
            instructions: DEFAULT_INSTRUCTIONS.to_owned(),
            timeout: Duration::from_secs(timeout_seconds),
            runtime,
        })
    }

    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = instructions.into();
        self
    }

    fn request(
        &self,
        lease: &RuntimeTaskLease,
        context: &AgentContext,
    ) -> Result<AgentOutcome, AgentWorkerError> {
        let input = serde_json::to_string(&AgentTaskRequest {
            schema_version: 1,
            context,
            task_kind: &lease.task_kind,
            payload: &lease.payload,
        })
        .map_err(|error| AgentWorkerError::Provider {
            message: format!("failed to encode task input: {error}"),
        })?;
        let request = CreateResponseArgs::default()
            .model(&self.model)
            .instructions(&self.instructions)
            .input(input)
            .max_output_tokens(1_024u32)
            .text(ResponseTextParam {
                format: TextResponseFormatConfiguration::JsonSchema(ResponseFormatJsonSchema {
                    name: "agent_outcome".to_owned(),
                    description: Some("The next durable Agent outcome.".to_owned()),
                    schema: outcome_schema(),
                    strict: Some(true),
                }),
                verbosity: None,
            })
            .build()
            .map_err(|error| AgentWorkerError::Provider {
                message: format!("failed to build provider request: {error}"),
            })?;

        let response = self.runtime.block_on(async {
            tokio::time::timeout(self.timeout, self.client.responses().create(request))
                .await
                .map_err(|_| AgentWorkerError::Provider {
                    message: format!(
                        "provider timed out after {} seconds",
                        self.timeout.as_secs()
                    ),
                })?
                .map_err(|error| AgentWorkerError::Provider {
                    message: error.to_string(),
                })
        })?;
        let output = response
            .output_text()
            .ok_or_else(|| AgentWorkerError::Provider {
                message: "provider response did not contain text output".to_owned(),
            })?;
        serde_json::from_str(&output).map_err(|error| AgentWorkerError::Provider {
            message: format!("provider returned invalid AgentOutcome JSON: {error}"),
        })
    }
}

impl AgentWorker for OpenAiAgentWorker {
    fn execute(
        &mut self,
        lease: &RuntimeTaskLease,
        session: &mut AgentSession<'_>,
    ) -> Result<AgentOutcome, AgentWorkerError> {
        let context = session.context(lease)?;
        self.request(lease, &context)
    }
}

fn outcome_schema() -> serde_json::Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": { "type": { "const": "completed" }, "result": {} },
                "required": ["type", "result"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "failed" },
                    "kind": {
                        "type": "string",
                        "enum": ["transient", "permanent", "crash"]
                    },
                    "message": { "type": "string" }
                },
                "required": ["type", "kind", "message"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": { "type": { "const": "checkpointed" }, "progress": {} },
                "required": ["type", "progress"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "proposed_plan" },
                    "proposal_id": { "type": "string" }
                },
                "required": ["type", "proposal_id"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": { "type": { "const": "waiting_for_approval" } },
                "required": ["type"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "crashed" },
                    "step": { "type": "integer", "minimum": 0 }
                },
                "required": ["type", "step"],
                "additionalProperties": false
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::outcome_schema;
    use crate::runtime::agent_worker::{AgentOutcome, ErrorKind};

    #[test]
    fn outcome_schema_has_stable_wire_kinds() {
        let schema = outcome_schema();
        assert_eq!(
            schema["oneOf"][0]["properties"]["type"]["const"],
            "completed"
        );
        assert_eq!(schema["oneOf"][0]["required"][0], "type");
    }

    #[test]
    fn agent_outcome_wire_shape_is_provider_compatible() {
        let outcome = serde_json::json!({
            "type": "failed",
            "kind": "transient",
            "message": "rate limited"
        });
        assert_eq!(
            serde_json::from_value::<AgentOutcome>(outcome).expect("outcome"),
            AgentOutcome::Failed {
                kind: ErrorKind::Transient,
                message: "rate limited".to_owned()
            }
        );
    }
}
