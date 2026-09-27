use std::error::Error;

use signaling_server::serve;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const LISTEN_ADDRESS: &str = "0.0.0.0:9000";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "info,signaling_server=debug",
        ))
        .try_init()?;
    let listener = match TcpListener::bind(LISTEN_ADDRESS).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(listen_address = LISTEN_ADDRESS, error = %error, "Falha ao abrir porta do servidor de sinalização");
            return Err(Box::new(error) as Box<dyn Error + Send + Sync>);
        }
    };
    tracing::info!(
        listen_address = LISTEN_ADDRESS,
        "Servidor de sinalização escutando"
    );
    tracing::info!("No Windows, use ipconfig para encontrar o IPv4 deste computador");
    tracing::info!("Configure os clientes com ws://<IP-IPv4-DO-COMPUTADOR>:9000");
    let (_shutdown_sender, shutdown_receiver) = oneshot::channel();
    serve(listener, shutdown_receiver, None).await?;
    Ok(())
}
