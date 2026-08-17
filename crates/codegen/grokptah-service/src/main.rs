use anyhow::Result;
use grokptah_service::{help_text, parse_args, run_service, StartupAction, SERVICE_VERSION};

#[tokio::main]
async fn main() -> Result<()> {
    match parse_args(std::env::args().skip(1))? {
        StartupAction::Run(config) => run_service(config).await,
        StartupAction::Help => {
            println!("{}", help_text());
            Ok(())
        }
        StartupAction::Version => {
            println!("grokptah-service {SERVICE_VERSION}");
            Ok(())
        }
    }
}
