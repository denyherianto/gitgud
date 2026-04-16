#[tokio::main]
async fn main() {
    if let Err(error) = gitgud::run().await {
        if let Some(exit) = error.downcast_ref::<gitgud::app::GitPassthroughExit>() {
            std::process::exit(exit.code());
        }

        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
