use std::error::Error;

use signaling_server::serve;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const LISTEN_ADDRESS: &str = "0.0.0.0:9000";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(LISTEN_ADDRESS).await?;
    println!("Servidor de sinalização escutando em {LISTEN_ADDRESS}.");
    println!("No Windows, use ipconfig para encontrar o IPv4 deste computador.");
    println!("Configure os clientes com ws://<IP-IPv4-DO-COMPUTADOR>:9000.");
    let (_shutdown_sender, shutdown_receiver) = oneshot::channel();
    serve(listener, shutdown_receiver, None).await?;
    Ok(())
}
