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
    net::TcpStream,
    prelude::*,
    sync::{
        mpsc::{self, UnboundedSender},
        oneshot,
    },
};

type WriterMessage = (RawNetworkMessage, Option<oneshot::Sender<()>>);
type EventMessage = (Event, Option<oneshot::Sender<()>>);

pub type EventReceiver = mpsc::UnboundedReceiver<EventMessage>;
pub type EventSender = mpsc::UnboundedSender<EventMessage>;

#[derive(Debug)]
pub enum Event {
    Message(NetworkMessage),
    Disconnected(Result<()>),
}

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
    mpsc::UnboundedSender<EventMessage>,
    mpsc::UnboundedReceiver<EventMessage>,
) {
    let (writer_tx, writer_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    tokio::spawn(run_connection(addr, stream, event_tx.clone(), writer_rx));

    (WriteHandle { writer_tx, network }, event_tx, event_rx)
}

async fn run_connection(
    addr: SocketAddr,
    mut stream: TcpStream,
    event_tx: EventSender,
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
                    let (tx, rx) = oneshot::channel();
                    event_tx.send((Event::Message(message.payload), Some(tx)))?;
                    let _ = rx.await;
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

    let _ = event_tx.send((Event::Disconnected(result), None));

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

        let (write_handle, event_tx, mut event_rx) =
            create_connection(addr, stream, Network::Testnet);

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
        let message = event_rx.recv().await;
        trace!("{:?}", message);
        assert!(matches!(
            message,
            Some((Event::Message(NetworkMessage::Version(_)), _))
        ));

        // completes immediately
        write_handle.queue_message(NetworkMessage::Verack);

        // read verack message
        let message = event_rx.recv().await;
        trace!("{:?}", message);
        assert!(matches!(
            message,
            Some((Event::Message(NetworkMessage::Verack), _))
        ));
    }
}
