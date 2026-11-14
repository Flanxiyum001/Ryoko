#[tokio::main]
async fn main() {
    if let Err(error) = ryoko_native::run_stdio().await {
        eprintln!("ryoko-lsp: {error}");
        std::process::exit(1);
    }
}
