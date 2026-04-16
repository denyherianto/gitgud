#[tokio::main]
async fn main() {
    if let Err(error) = git_buddy::run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
