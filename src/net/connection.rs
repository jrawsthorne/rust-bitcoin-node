use super::{codec::BitcoinCodec, Peer};
use bitcoin::{network::message::NetworkMessage, Network};
use failure::{format_err, Error};
use futures::StreamExt;
use futures_util::SinkExt;
use std::net::Shutdown;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::{net::TcpStream, sync::mpsc};
use tokio_util::codec::Framed;

/// Trait that handles TCP stream events
#[async_trait::async_trait]
pub trait ConnectionListener: Send + Sync + 'static {
    async fn handle_connect(&self);
    async fn handle_close(&self);
    async fn handle_packet(&self, packet: NetworkMessage);
    async fn handle_error(&self, error: &Error);
}

pub struct Connection;

impl Connection {
    pub fn from_outbound(
        network: Network,
        peer: Arc<Peer>,
        rx: mpsc::UnboundedReceiver<NetworkMessage>,
        close_connection_rx: mpsc::UnboundedReceiver<()>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(peer.addr)).await
            {
                Ok(Ok(stream)) => {
                    peer.handle_connect().await;
                    Connection::run(network, peer, rx, stream, close_connection_rx).await;
                }
                Ok(Err(error)) => {
                    let error = format_err!("connection error: {}", error);
                    peer.handle_error(&error).await;
                }
                Err(_) => {
                    let error = format_err!("connection error: timeout");
                    peer.handle_error(&error).await;
                }
            }
        })
    }

    pub fn from_inbound(
        network: Network,
        peer: Arc<Peer>,
        stream: TcpStream,
        rx: mpsc::UnboundedReceiver<NetworkMessage>,
        close_connection_rx: mpsc::UnboundedReceiver<()>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            Connection::run(network, peer, rx, stream, close_connection_rx).await;
        })
    }

    async fn run(
        network: Network,
        peer: Arc<Peer>,
        mut rx: mpsc::UnboundedReceiver<NetworkMessage>,
        stream: TcpStream,
        mut close_connection_rx: mpsc::UnboundedReceiver<()>,
    ) {
        let stream = Framed::new(stream, BitcoinCodec::new(network));
        let (mut w, mut r) = stream.split();

        let recv_message = async {
            while let Some(message) = rx.recv().await {
                if let Err(error) = w.send(message).await {
                    peer.handle_error(&error.into()).await;
                    break;
                }
            }
        };

        let recv_socket = async {
            while let Some(message) = r.next().await {
                match message {
                    Ok(message) => peer.handle_packet(message).await,
                    Err(error) => {
                        peer.handle_error(&error.into()).await;
                        return;
                    }
                }
            }
            // peer cleanly closed the connection
            peer.handle_close().await;
        };

        let recv_close = async {
            close_connection_rx.next().await;
        };

        tokio::select! {
            _ = recv_message => (),
            _ = recv_socket => (),
            _ = recv_close => ()
        };

        // shutdown stream cleanly ignoring error
        // rx will be closed on drop

        let _ = w.reunite(r).unwrap().get_mut().shutdown(Shutdown::Both);
    }
}
