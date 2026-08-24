use serde::Serialize;

use crate::app::AppError;

#[derive(Debug, Clone, Copy)]
pub struct OutputMode {
    json: bool,
    quiet: bool,
}

impl OutputMode {
    pub const fn new(json: bool, quiet: bool) -> Self {
        Self { json, quiet }
    }

    pub fn print_json_or_human<T: Serialize>(
        self,
        value: &T,
        human: String,
    ) -> Result<(), AppError> {
        if self.json {
            println!("{}", serde_json::to_string_pretty(value)?);
        } else if !self.quiet {
            println!("{human}");
        }
        Ok(())
    }
}

pub fn render_error(error: &AppError, json: bool) -> String {
    if json {
        let payload = serde_json::json!({
            "error": {
                "kind": error.kind(),
                "message": error.to_string(),
            }
        });
        serde_json::to_string(&payload).unwrap_or_else(|_| {
            r#"{"error":{"kind":"output_error","message":"failed to render error"}}"#.to_owned()
        })
    } else {
        error.to_string()
    }
}
