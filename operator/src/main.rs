#[tokio::main]
async fn main() -> anyhow::Result<()> {
    oabctl::run_cli().await
}
