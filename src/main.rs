use std::process::ExitCode;

use agent_stride::{app::App, cli::Cli, output::render_error};

fn main() -> ExitCode {
    let cli = Cli::parse_args();
    let json = cli.json;

    match App::run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", render_error(&error, json));
            ExitCode::from(error.exit_code())
        }
    }
}
