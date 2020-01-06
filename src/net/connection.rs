use super::{codec::BitcoinCodec, Peer};
use bitcoin::network::message::NetworkMessage;
use failure::{format_err, Error};
use futures::future::select;
use futures_util::{SinkExt, StreamExt};
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

#[derive(Debug)]
pub enum ConnectionMessage {
    Broadcast(NetworkMessage),
    Close,
}

pub struct Connection;

impl Connection {
    pub fn from_outbound(peer: Arc<Peer>, rx: mpsc::Receiver<ConnectionMessage>) -> JoinHandle<()> {
        tokio::spawn(async move {
            match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(peer.addr)).await
            {
                Ok(Ok(stream)) => {
                    peer.handle_connect().await;
                    Connection::run(peer, rx, stream).await;
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
        peer: Arc<Peer>,
        stream: TcpStream,
        rx: mpsc::Receiver<ConnectionMessage>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            Connection::run(peer, rx, stream).await;
        })
    }

    async fn run(peer: Arc<Peer>, mut rx: mpsc::Receiver<ConnectionMessage>, stream: TcpStream) {
        let stream = Framed::new(stream, BitcoinCodec::default());
        let (mut w, mut r) = stream.split();

        let recv_message = async {
            while let Some(message) = rx.recv().await {
                match message {
                    ConnectionMessage::Broadcast(message) => {
                        if let Err(error) = w.send(message).await {
                            peer.handle_error(&error.into()).await;
                            break;
                        }
                    }
                    ConnectionMessage::Close => {
                        // TODO: use separate channel to stop a blocked write from preventing close
                        break;
                    }
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

        select(Box::pin(recv_message), Box::pin(recv_socket)).await;

        // shutdown stream cleanly ignoring error
        // rx will be closed on drop

        let _ = w.reunite(r).unwrap().get_mut().shutdown(Shutdown::Both);
    }
}
