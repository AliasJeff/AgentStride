#[derive(Debug, Clone)]
pub struct ExecutionPolicy {
    allowed_commands: Vec<String>,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            allowed_commands: vec!["cargo".to_owned(), "git".to_owned(), "rustc".to_owned()],
        }
    }
}

impl ExecutionPolicy {
    pub fn check(&self, command: &str) -> Result<(), crate::runner::RunnerError> {
        let program = std::path::Path::new(command)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(command);
        if self.allowed_commands.iter().any(|value| value == program) {
            Ok(())
        } else {
            Err(crate::runner::RunnerError::command_denied(command))
        }
    }
}
