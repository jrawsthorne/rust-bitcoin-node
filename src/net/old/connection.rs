use super::Peer;
use bitcoin::{
    consensus::{deserialize_partial, serialize},
    network::message::{NetworkMessage, RawNetworkMessage},
    Network,
};
use bytes::Buf;
use bytes::BytesMut;
use failure::format_err;
use log::debug;
use std::sync::Arc;
use std::time::Duration;
use tokio::prelude::*;
use tokio::task::JoinHandle;
use tokio::{net::TcpStream, sync::mpsc};

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
                    peer.handle_connect();
                    Connection::run(network, peer, rx, stream, close_connection_rx).await;
                }
                Ok(Err(error)) => {
                    let error = format_err!("connection error: {}", error);
                    peer.handle_error(&error);
                }
                Err(_) => {
                    let error = format_err!("connection error: timeout");
                    peer.handle_error(&error);
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
        mut stream: TcpStream,
        mut close_connection_rx: mpsc::UnboundedReceiver<()>,
    ) {
        stream.set_nodelay(true).unwrap();

        let (reader, writer) = stream.split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        let recv_message = async {
            while let Some(message) = rx.recv().await {
                let bytes = serialize(&RawNetworkMessage {
                    magic: network.magic(),
                    payload: message,
                });
                if let Err(error) = writer.write_all(&bytes).await {
                    peer.handle_error(&error.into());
                    break;
                }
                if let Err(error) = writer.flush().await {
                    peer.handle_error(&error.into());
                    break;
                }
            }
        };

        let recv_socket = async {
            let mut buf = BytesMut::with_capacity(8_000);
            loop {
                match reader.read_buf(&mut buf).await {
                    Ok(i) => {
                        if i == 0 {
                            // clean shutdown
                            return peer.handle_close();
                        }
                    }
                    Err(error) => {
                        peer.handle_error(&error.into());
                        return;
                    }
                }
                match deserialize_partial::<RawNetworkMessage>(buf.bytes()) {
                    Ok((message, index)) => {
                        buf.advance(index);
                        peer.handle_packet(message.payload);
                    }
                    Err(bitcoin::consensus::encode::Error::Io(ref err))
                        if err.kind() == io::ErrorKind::UnexpectedEof => {}
                    Err(error) => {
                        peer.handle_error(&error.into());
                        return;
                    }
                }
            }
        };

        let recv_close = async {
            close_connection_rx.recv().await;
        };

        tokio::select! {
            _ = recv_message => (),
            _ = recv_socket => (),
            _ = recv_close => ()
        };

        // stream shutdown when r & w dropped
    }
}
