use {
    crate::{
        constants::{DISCOVERY_INTERVAL, MAX_INV},
        peer::{Peer, PeerOptions, Sender},
    },
    bitcoin::{
        consensus::encode,
        hashes::sha256d,
        network::{
            address::Address,
            constants::Network,
            message::NetworkMessage,
            message_blockdata::{InvType, Inventory},
        },
    },
    failure::{err_msg, Error},
    log::debug,
    std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration},
    tokio::{
        net::{tcp::Incoming, TcpListener, TcpStream},
        prelude::*,
        sync::{mpsc, Mutex},
        timer::Interval,
    },
};

pub type PoolSender = mpsc::UnboundedSender<(NetworkMessage, SocketAddr)>;
pub type PoolReceiver = mpsc::UnboundedReceiver<(NetworkMessage, SocketAddr)>;

use futures::Poll;
use std::{pin::Pin, task::Context};

pub struct Pool {
    peers: Arc<Mutex<HashMap<SocketAddr, Sender>>>,
    receiver: PoolReceiver,
    sender: PoolSender,
    network: Network,
    outbound: Vec<SocketAddr>,
    incoming: Incoming,
    hosts: Vec<SocketAddr>,
    discovery_timer: Interval,
}

pub struct PoolOptions {
    network: Network,
}

impl Default for PoolOptions {
    fn default() -> PoolOptions {
        PoolOptions {
            network: Network::Bitcoin,
        }
    }
}

impl Pool {
    pub async fn connect(outbound: Vec<SocketAddr>, options: PoolOptions) -> Result<Pool, Error> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        debug!("Listening on {}", listener.local_addr()?);
        Ok(Pool {
            peers: Arc::new(Mutex::new(HashMap::new())),
            receiver,
            sender,
            network: options.network,
            outbound: vec![],
            incoming: listener.incoming(),
            hosts: outbound,
            discovery_timer: Interval::new_interval(Duration::from_millis(DISCOVERY_INTERVAL)),
        })
    }

    fn connect_to_addr(&self, addr: SocketAddr) {
        let peers = self.peers.clone();
        let pool = self.sender.clone();
        let addr = addr.clone();
        let network = self.network.clone();
        tokio::spawn(async move {
            match Peer::connect(&addr, PeerOptions::default().pool(pool).network(network)).await {
                Ok((peer, sender)) => create_peer(addr, peer, sender, peers).await,
                Err(err) => debug!("Couldn't connect to peer: {}", err),
            }
        });
    }

    fn add_outbound(&mut self) {
        if let Some(addr) = self.hosts.pop() {
            self.connect_to_addr(addr);
        }
    }

    async fn refill_peers(&mut self) {
        let peers_len;
        {
            peers_len = self.peers.lock().await.len();
        }
        let need = 8 - peers_len;
        if need <= 0 {
            return;
        }
        debug!("Refilling peers ({}/{})", peers_len, 8);
        for _ in 0..need {
            self.add_outbound();
        }
    }

    async fn handle_inv(&mut self, items: Vec<Inventory>, peer: SocketAddr) -> Result<(), Error> {
        if items.len() > MAX_INV {
            // ban
            return Err(err_msg("Peer sent inv with too many items, ban"));
        }

        let mut blocks = vec![];
        let mut txs = vec![];

        for item in &items {
            match item.inv_type {
                InvType::Block => blocks.push(item.hash),
                InvType::Transaction => txs.push(item.hash),
                _ => debug!("Peer sent an unknown inv type: {:?}", item.inv_type),
            }
        }

        debug!(
            "Received inv packet with {} items: blocks={} txs={}",
            items.len(),
            blocks.len(),
            txs.len()
        );

        if blocks.len() > 0 {
            self.handle_block_inv(blocks, &peer);
        }
        if txs.len() > 0 {
            self.handle_tx_inv(txs, &peer).await?;
        }
        Ok(())
    }

    fn handle_block_inv(&mut self, hashes: Vec<sha256d::Hash>, peer: &SocketAddr) {}

    async fn handle_tx_inv(
        &mut self,
        hashes: Vec<sha256d::Hash>,
        peer_addr: &SocketAddr,
    ) -> Result<(), Error> {
        let mut peer;
        {
            match self.peers.lock().await.get(peer_addr) {
                Some(p) => peer = p.clone(),
                None => return Err(err_msg("Handled inv from disconnected peer")),
            }
        }
        let mut items = vec![];
        for hash in hashes {
            items.push(Inventory {
                inv_type: InvType::WitnessTransaction,
                hash,
            });
        }
        peer.send(NetworkMessage::GetData(items)).await?;
        Ok(())
    }

    async fn handle_addr(&mut self, addrs: Vec<(u32, Address)>) -> Result<(), Error> {
        let peers = self.peers.lock().await;
        for addr in addrs {
            if let Ok(socket_addr) = addr.1.socket_addr() {
                if peers.contains_key(&socket_addr) {
                    continue;
                }
                self.hosts.push(socket_addr);
            }
        }
        Ok(())
    }

    fn accept_inbound(&mut self, stream: TcpStream) -> Result<(), Error> {
        let addr = stream.peer_addr()?;
        debug!("New connection {} {:?}", addr, stream);
        let peers = self.peers.clone();
        let pool = self.sender.clone();
        tokio::spawn(async move {
            match Peer::from_stream(stream, PeerOptions::default().pool(pool)) {
                Ok((peer, sender)) => create_peer(addr, peer, sender, peers).await,
                Err(err) => debug!("Couldn't connect to peer: {}", err),
            }
        });
        Ok(())
    }

    pub async fn start(&mut self) -> Result<(), Error> {
        self.refill_peers().await;
        while let Some(result) = self.next().await {
            match result {
                // accepted new connection
                Ok(Message::Accept(stream)) => self.accept_inbound(stream)?,
                Ok(Message::Packet((message, peer_addr))) => match message {
                    NetworkMessage::Addr(addrs) => self.handle_addr(addrs).await?,
                    NetworkMessage::Inv(items) => self.handle_inv(items, peer_addr).await?,
                    _ => (),
                },
                Ok(Message::Discover) => {
                    self.refill_peers().await;
                }
                _ => (),
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
pub enum Message {
    Packet((NetworkMessage, SocketAddr)),
    Accept(TcpStream),
    Discover,
}

async fn create_peer(
    addr: SocketAddr,
    mut peer: Peer,
    sender: Sender,
    peers: Arc<Mutex<HashMap<SocketAddr, Sender>>>,
) {
    {
        peers.lock().await.insert(addr, sender);
    }
    match peer.start().await {
        Ok(_) => debug!("Peer {} disconnected gracefully", addr),
        Err(err) => {
            peers.lock().await.remove(&addr);
            debug!("Peer {} disconnected with an error {}", addr, err);
        }
    }
}

impl Stream for Pool {
    type Item = Result<Message, encode::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Poll::Ready(Some(Ok(stream))) = self.incoming.poll_next_unpin(cx) {
            return Poll::Ready(Some(Ok(Message::Accept(stream))));
        }

        if let Poll::Ready(_) = self.discovery_timer.poll_next_unpin(cx) {
            return Poll::Ready(Some(Ok(Message::Discover)));
        }

        let result: Option<_> = futures::ready!(self.receiver.poll_next_unpin(cx));

        Poll::Ready(match result {
            Some(message) => Some(Ok(Message::Packet(message))),
            None => None,
        })
    }
}
