use anyhow::{bail, ensure, Result};
use bitcoin::network::{
    message::NetworkMessage, message_network::VersionMessage, stream_reader::StreamReader,
};
use std::net::TcpStream;

pub enum TXRelay {
    Txid,
    Wtxid,
}

pub enum AddrRelay {
    V1,
    V2,
}

pub struct Peer {
    stream: TcpStream,
    version: Option<VersionMessage>,
    handshake: bool,
    tx_relay: TXRelay,
    addr_relay: AddrRelay,
    expected_nonce: Option<u64>,
}

struct Connection {
    reader: StreamReader<TcpStream>,
    writer: TcpStream,
}

impl Connection {
    fn new(stream: TcpStream) -> Self {
        Self {
            writer: stream.try_clone().unwrap(),
            reader: StreamReader::new(stream, Some(4096)),
        }
    }

    fn read_next(&mut self) -> Result<NetworkMessage> {
        self.reader.read_next()
    }
}

impl Peer {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            version: None,
            handshake: false,
            tx_relay: TXRelay::Txid,
            addr_relay: AddrRelay::V1,
            expected_nonce: None,
        }
    }

    fn handle_message(&mut self, message: NetworkMessage) -> Result<()> {
        match message {
            NetworkMessage::Version(version) => {
                ensure!(self.version.is_none(), "duplicate version");
                self.version = Some(version);
            }
            NetworkMessage::Verack => {
                ensure!(!self.handshake, "duplicate verack");
                self.handshake = true;
            }
            NetworkMessage::Ping(nonce) => {
                // send pong
            }
            NetworkMessage::Pong(nonce) => {
                ensure!(self.expected_nonce == Some(nonce), "unexpected pong");
                self.expected_nonce = None;
            }
            NetworkMessage::WtxidRelay => self.tx_relay = TXRelay::Wtxid,
            NetworkMessage::SendAddrV2 => self.addr_relay = AddrRelay::V2,
            _ => {}
        }
        Ok(())
    }
}

fn main() -> Result<()> {
    let stream = TcpStream::connect("hetzner:8333")?;
    let mut connection = Connection::new(stream);
    Ok(())
}

// System shutdown
// Error reading/writing/processing message
