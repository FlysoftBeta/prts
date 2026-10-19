#[tokio::main]
async fn main() {
    if let Err(error) = prtsd::daemon::run().await {
        eprintln!("prtsd: {error}");
        std::process::exit(1);
    }
}
