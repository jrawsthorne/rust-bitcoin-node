use anyhow::Result;
use bitcoin::{
    consensus::encode,
    consensus::{deserialize_partial, Encodable},
    network::message::{NetworkMessage, RawNetworkMessage},
    Network,
};
use bytes::{Buf, BytesMut};
use log::trace;
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{
        mpsc::{self, UnboundedSender},
        oneshot,
    },
};

type WriterMessage = (RawNetworkMessage, Option<oneshot::Sender<()>>);

pub type MessageReceiver = mpsc::Receiver<NetworkMessage>;
pub type MessageSender = mpsc::Sender<NetworkMessage>;

pub type DisconnectMessage = Result<()>;
pub type DisconnectReceiver = mpsc::UnboundedReceiver<DisconnectMessage>;
pub type DisconnectSender = mpsc::UnboundedSender<DisconnectMessage>;

pub struct WriteHandle {
    writer_tx: UnboundedSender<WriterMessage>,
    network: Network,
}

impl WriteHandle {
    pub fn queue_message(&self, message: NetworkMessage) {
        let _ = self.writer_tx.send((
            RawNetworkMessage {
                payload: message,
                magic: self.network.magic(),
            },
            None,
        ));
    }

    pub async fn write_message(&self, message: NetworkMessage) {
        let (done_tx, done_rx) = oneshot::channel();
        let _ = self.writer_tx.send((
            RawNetworkMessage {
                payload: message,
                magic: self.network.magic(),
            },
            Some(done_tx),
        ));
        let _ = done_rx.await;
    }
}

// TODO: Async stream for event_rx
pub fn create_connection(
    addr: SocketAddr,
    stream: TcpStream,
    network: Network,
) -> (
    WriteHandle,
    MessageReceiver,
    (DisconnectSender, DisconnectReceiver),
) {
    let (writer_tx, writer_rx) = mpsc::unbounded_channel();
    let (message_tx, message_rx) = mpsc::channel(1);
    let (disconnect_tx, disconnect_rx) = mpsc::unbounded_channel();

    tokio::spawn(run_connection(
        addr,
        stream,
        message_tx,
        disconnect_tx.clone(),
        writer_rx,
    ));

    (
        WriteHandle { writer_tx, network },
        message_rx,
        (disconnect_tx, disconnect_rx),
    )
}

async fn run_connection(
    addr: SocketAddr,
    mut stream: TcpStream,
    message_tx: MessageSender,
    disconnect_tx: DisconnectSender,
    mut writer_rx: mpsc::UnboundedReceiver<WriterMessage>,
) {
    let (mut reader, mut writer) = stream.split();

    let writer = async {
        let mut bytes = Vec::new();
        loop {
            if let Some((message, done_tx)) = writer_rx.recv().await {
                // encode message
                message
                    .consensus_encode(&mut bytes)
                    .expect("writing to a vec can't fail");

                // write message
                writer.write_all(&bytes).await?;

                writer.flush().await?;

                bytes.clear();

                // inform done_tx
                if let Some(done_tx) = done_tx {
                    let _ = done_tx.send(());
                }

                trace!("sent {} ({})", message.cmd(), addr);
            } else {
                return Ok(());
            }
        }
    };

    let reader = async {
        let mut buf = BytesMut::new();
        loop {
            match deserialize_partial::<RawNetworkMessage>(&buf[..]) {
                Ok((message, n_bytes)) => {
                    buf.advance(n_bytes);
                    trace!("received {} ({})", message.cmd(), addr);
                    message_tx.send(message.payload).await?;
                }
                Err(error) => {
                    use encode::Error;
                    use std::io::ErrorKind;

                    if matches!(&error, Error::Io(error) if error.kind() == ErrorKind::UnexpectedEof)
                    {
                        let bytes_read = reader.read_buf(&mut buf).await?;
                        if bytes_read == 0 {
                            return Ok(());
                        }
                    } else {
                        return Err(error.into());
                    }
                }
            }
        }
    };

    let result;

    tokio::select! {
        writer_result = writer => {
            result = writer_result;
        }
        reader_result = reader => {
            result = reader_result;
        }
    }

    let _ = disconnect_tx.send(result);

    trace!("connection disconnected {}", addr);
}

#[cfg(test)]
mod test {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use bitcoin::network::{constants::ServiceFlags, message_network::VersionMessage, Address};

    use super::*;

    #[tokio::test]
    async fn handshake_test() {
        env_logger::builder().format_timestamp_millis().init();

        let stream = TcpStream::connect("hetzner:18333").await.unwrap();
        let addr = stream.peer_addr().unwrap();

        let (write_handle, mut message_rx, _) = create_connection(addr, stream, Network::Testnet);

        let version = VersionMessage {
            version: 70015,
            services: ServiceFlags::WITNESS
                | ServiceFlags::BLOOM
                | ServiceFlags::NETWORK
                | ServiceFlags::COMPACT_FILTERS
                | ServiceFlags::NETWORK_LIMITED,
            timestamp: 0,
            receiver: Address::new(&addr, ServiceFlags::NONE),
            // TODO: Fill in our address from discover
            // TODO: Fill in services if we got the address from another peers addr
            sender: Address::new(
                &SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
                ServiceFlags::NONE,
            ),
            // TODO: Generate random nonce from pool that tracks nonces for all peers to ensure we don't connect to ourselves
            nonce: 0,
            user_agent: "/rust_bitcoin_node:0.1.0/".to_string(),
            start_height: 0,
            relay: true,
        };

        // waits for message to be written to os buffers before returning
        write_handle
            .write_message(NetworkMessage::Version(version))
            .await;

        // read version message
        let message = message_rx.recv().await;
        trace!("{:?}", message);
        assert!(matches!(message, Some(NetworkMessage::Version(_))));

        // completes immediately
        write_handle.queue_message(NetworkMessage::Verack);

        // read verack message
        let message = message_rx.recv().await;
        trace!("{:?}", message);
        assert!(matches!(message, Some(NetworkMessage::Verack)));
    }
}
