use std::{
    io::Write,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use async_openai::{
    Client,
    config::OpenAIConfig,
    types::responses::{
        CreateResponseArgs, ResponseFormatJsonSchema, ResponseTextParam,
        TextResponseFormatConfiguration,
    },
};
use serde::{Deserialize, Serialize};

use crate::domain::Issue;

const COMPACTION_INSTRUCTION: &str = "Summarize this closed engineering issue for future agents. Preserve conclusions, decisions, changes, verification evidence, and unresolved work. The summary must be shorter than the input. Return only the requested JSON object.";

#[derive(Debug, Clone, Serialize)]
pub struct SummaryRequest<'a> {
    pub issue: &'a Issue,
    pub instruction: &'static str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SummaryResponse {
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryResult {
    pub summary: String,
    pub provider: String,
    pub model: String,
    pub duration_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

pub trait Summarizer {
    fn summarize(&self, issue: &Issue) -> Result<SummaryResult, CompactorError>;
}

#[derive(Debug, thiserror::Error)]
pub enum CompactorError {
    #[error("no external compaction command configured; use --command or AG_COMPACTION_COMMAND")]
    MissingCommand,
    #[error("OPENAI_API_KEY is not configured")]
    MissingApiKey,
    #[error("failed to start compaction command: {0}")]
    Start(#[from] std::io::Error),
    #[error("compaction command exited unsuccessfully: {0}")]
    Failed(String),
    #[error("compaction command timed out after {0} seconds")]
    Timeout(u64),
    #[error("compaction command returned invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("compaction command returned an empty summary")]
    EmptySummary,
    #[error("OpenAI request failed: {0}")]
    OpenAi(String),
    #[error("OpenAI response did not contain text output")]
    MissingOutput,
}

#[derive(Debug)]
pub struct ExternalCompactor {
    command: String,
    args: Vec<String>,
    timeout: Duration,
    retries: usize,
}

impl ExternalCompactor {
    pub fn from_config(command: Option<String>, args: Vec<String>) -> Result<Self, CompactorError> {
        Self::with_options(command, args, 120, 2)
    }

    pub fn with_options(
        command: Option<String>,
        args: Vec<String>,
        timeout_seconds: u64,
        retries: usize,
    ) -> Result<Self, CompactorError> {
        let command = command
            .or_else(|| std::env::var("AG_COMPACTION_COMMAND").ok())
            .filter(|value| !value.trim().is_empty())
            .ok_or(CompactorError::MissingCommand)?;
        Ok(Self {
            command,
            args,
            timeout: Duration::from_secs(timeout_seconds),
            retries,
        })
    }

    pub fn summarize(&self, issue: &Issue) -> Result<SummaryResult, CompactorError> {
        let mut last_error = None;
        for attempt in 0..=self.retries {
            match self.summarize_once(issue) {
                Ok(result) => return Ok(result),
                Err(error) if attempt < self.retries => {
                    last_error = Some(error);
                    thread::sleep(Duration::from_millis(100 * (attempt as u64 + 1)));
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.expect("at least one summarizer attempt ran"))
    }

    fn summarize_once(&self, issue: &Issue) -> Result<SummaryResult, CompactorError> {
        let request = serde_json::to_vec(&SummaryRequest {
            issue,
            instruction: COMPACTION_INSTRUCTION,
        })?;
        let started = Instant::now();
        let mut child = Command::new(&self.command)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("stdin was configured")
            .write_all(&request)?;
        let deadline = Instant::now() + self.timeout;
        loop {
            if child.try_wait()?.is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CompactorError::Timeout(self.timeout.as_secs()));
            }
            thread::sleep(Duration::from_millis(25));
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(CompactorError::Failed(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let response: SummaryResponse = serde_json::from_slice(&output.stdout)?;
        if response.summary.trim().is_empty() {
            return Err(CompactorError::EmptySummary);
        }
        Ok(SummaryResult {
            summary: response.summary,
            provider: "external".to_owned(),
            model: "external".to_owned(),
            duration_ms: started.elapsed().as_millis() as u64,
            input_tokens: None,
            output_tokens: None,
        })
    }
}

impl Summarizer for ExternalCompactor {
    fn summarize(&self, issue: &Issue) -> Result<SummaryResult, CompactorError> {
        ExternalCompactor::summarize(self, issue)
    }
}

#[derive(Debug)]
pub struct OpenAiCompactor {
    client: Client<OpenAIConfig>,
    model: String,
    timeout: Duration,
    retries: usize,
    runtime: tokio::runtime::Runtime,
}

impl OpenAiCompactor {
    pub fn from_env(
        model: String,
        timeout_seconds: u64,
        retries: usize,
    ) -> Result<Self, CompactorError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or(CompactorError::MissingApiKey)?;
        Self::with_options(api_key, model, timeout_seconds, retries)
    }

    pub fn with_options(
        api_key: String,
        model: String,
        timeout_seconds: u64,
        retries: usize,
    ) -> Result<Self, CompactorError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| CompactorError::OpenAi(error.to_string()))?;
        Ok(Self {
            client: Client::with_config(OpenAIConfig::new().with_api_key(api_key)),
            model,
            timeout: Duration::from_secs(timeout_seconds),
            retries,
            runtime,
        })
    }

    fn summarize_once(&self, issue: &Issue) -> Result<SummaryResult, CompactorError> {
        let prompt = serde_json::to_string(&SummaryRequest {
            issue,
            instruction: COMPACTION_INSTRUCTION,
        })?;
        let request = CreateResponseArgs::default()
            .model(&self.model)
            .instructions(COMPACTION_INSTRUCTION)
            .input(prompt)
            .max_output_tokens(512u32)
            .text(ResponseTextParam {
                format: TextResponseFormatConfiguration::JsonSchema(ResponseFormatJsonSchema {
                    name: "compaction_summary".to_owned(),
                    description: Some(
                        "A concise reversible summary of an engineering issue.".to_owned(),
                    ),
                    schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "summary": { "type": "string" }
                        },
                        "required": ["summary"],
                        "additionalProperties": false
                    }),
                    strict: Some(true),
                }),
                verbosity: None,
            })
            .build()
            .map_err(|error| CompactorError::OpenAi(error.to_string()))?;
        let started = Instant::now();
        let response = self.runtime.block_on(async {
            tokio::time::timeout(self.timeout, self.client.responses().create(request))
                .await
                .map_err(|_| CompactorError::Timeout(self.timeout.as_secs()))?
                .map_err(|error| CompactorError::OpenAi(error.to_string()))
        })?;
        let output = response
            .output_text()
            .ok_or(CompactorError::MissingOutput)?;
        let parsed: SummaryResponse = serde_json::from_str(&output)?;
        if parsed.summary.trim().is_empty() {
            return Err(CompactorError::EmptySummary);
        }
        Ok(SummaryResult {
            summary: parsed.summary,
            provider: "openai".to_owned(),
            model: response.model,
            duration_ms: started.elapsed().as_millis() as u64,
            input_tokens: response
                .usage
                .as_ref()
                .map(|usage| u64::from(usage.input_tokens)),
            output_tokens: response
                .usage
                .as_ref()
                .map(|usage| u64::from(usage.output_tokens)),
        })
    }
}

impl Summarizer for OpenAiCompactor {
    fn summarize(&self, issue: &Issue) -> Result<SummaryResult, CompactorError> {
        let mut last_error = None;
        for attempt in 0..=self.retries {
            match self.summarize_once(issue) {
                Ok(result) => return Ok(result),
                Err(error) if attempt < self.retries => {
                    last_error = Some(error);
                    thread::sleep(Duration::from_millis(100 * (attempt as u64 + 1)));
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.expect("at least one summarizer attempt ran"))
    }
}
