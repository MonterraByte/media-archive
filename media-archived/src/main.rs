#![forbid(unsafe_code)]

mod server;

use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

use crate::server::{CreateStateError, State};

#[derive(Debug, Parser)]
struct Args {
    /// Address to listen on
    #[arg(short, long, default_value_t = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 47874))]
    listen: SocketAddr,
    /// Path to the media archive
    path: PathBuf,
}

async fn serve(addr: SocketAddr, path: PathBuf) -> Result<(), ServeError> {
    let listener = TcpListener::bind(addr).await.map_err(ServeError::Socket)?;
    let state = State::new(path).await.map_err(ServeError::CreateState)?;
    let app = server::router().with_state(Arc::new(state));
    axum::serve(listener, app)
        .await
        .map_err(ServeError::Serve)
        .unwrap();
    Ok(())
}

#[derive(Debug, Error)]
enum ServeError {
    #[error("failed to create state: {0}")]
    CreateState(CreateStateError),
    #[error("failed to open TCP socket")]
    Socket(io::Error),
    #[error("{0}")]
    Serve(io::Error),
}

fn main() -> Result<(), ServerError> {
    let args = Args::parse();
    Runtime::new()
        .map_err(ServerError::CreateRuntime)?
        .block_on(serve(args.listen, args.path))
        .map_err(ServerError::Serve)
}

#[derive(Debug, Error)]
enum ServerError {
    #[error("failed to create Tokio runtime: {0}")]
    CreateRuntime(io::Error),
    #[error("{0}")]
    Serve(ServeError),
}
