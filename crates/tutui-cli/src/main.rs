use tutui::process::ExternalProcess;
use tutui::Registry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tutui::cli::main(Registry::new().register(ExternalProcess)).await
}
