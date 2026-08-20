use clap::error::ErrorKind;
use clap::Parser;
use grokptah_certification_lab::cli::{execute, Cli, ExitClass};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let code = match Cli::try_parse() {
        Ok(cli) => execute(cli).await,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            if error.print().is_ok() {
                ExitClass::Passed
            } else {
                ExitClass::HarnessFailure
            }
        }
        Err(_) => {
            eprintln!("grokptah-cert: invalid_command_line");
            ExitClass::InvalidConfiguration
        }
    } as i32;
    std::process::exit(code);
}
