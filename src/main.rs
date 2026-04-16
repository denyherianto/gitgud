#[tokio::main]
async fn main() {
    if let Err(error) = gitgud::run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
