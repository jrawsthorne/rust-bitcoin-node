use {
    crate::{
        codec::BitcoinCodec,
        constants::{
            Services, CONNECT_TIMEOUT, HANDSHAKE_TIMEOUT, LOCAL_SERVICES, MIN_VERSION,
            PING_INTERVAL, USER_AGENT,
        },
        helper::now,
    },
    bitcoin::{
        consensus::encode,
        network::{
            address::Address,
            constants::{Network, PROTOCOL_VERSION},
            message::NetworkMessage,
            message_network::VersionMessage,
        },
    },
    failure::{err_msg, Error},
    futures::Poll,
    log::debug,
    rand::random,
    std::{
        cmp::min,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        pin::Pin,
        task::Context,
        time::Duration,
    },
    tokio::{codec::Framed, net::TcpStream, prelude::*, timer::Interval},
};

pub struct Peer {
    socket: Framed<TcpStream, BitcoinCodec>,
    pub addr: SocketAddr,
    version: Option<VersionMessage>, // version packet from peer
    ping_timer: Interval,            // notify when to send ping
    nonce: Option<u64>,              // nonce we expect back from pong
    last_ping: Option<u64>,
    last_pong: Option<u64>,
    min_ping: Option<u64>, // Lowest ping time seen.
}

pub struct PeerOptions {
    pub network: Network, // e.g. main, testnet or regtest
}

impl Default for PeerOptions {
    fn default() -> PeerOptions {
        PeerOptions {
            network: Network::Bitcoin,
        }
    }
}

impl Peer {
    pub async fn connect(addr: &SocketAddr, options: PeerOptions) -> Result<Peer, failure::Error> {
        // connect to given address, bail if takes more than 10 seconds
        debug!("connecting to {}", addr);
        let stream = TcpStream::connect(addr)
            .timeout(Duration::from_secs(CONNECT_TIMEOUT))
            .await??;
        debug!("connected to peer {}", addr);
        Peer::from_stream(stream, options)
    }

    pub fn from_stream(socket: TcpStream, options: PeerOptions) -> Result<Peer, failure::Error> {
        let socket = Framed::new(socket, BitcoinCodec::new(options.network));
        let addr = socket.get_ref().peer_addr()?;
        Ok(Peer {
            socket,
            addr,
            version: None,
            ping_timer: Interval::new_interval(Duration::from_secs(PING_INTERVAL)),
            nonce: None,
            last_ping: None,
            min_ping: None,
            last_pong: None,
        })
    }

    async fn send(&mut self, message: NetworkMessage) -> Result<(), Error> {
        match message {
            NetworkMessage::Version(_) => debug!("sent version to {}", self.addr),
            NetworkMessage::Verack => debug!("sent verack to {}", self.addr),
            NetworkMessage::GetData(_) => debug!("sent getdata to {}", self.addr),
            NetworkMessage::GetHeaders(_) => debug!("sent getheaders to {}", self.addr),
            NetworkMessage::SendHeaders => debug!("sent sendheaders to {}", self.addr),
            NetworkMessage::Ping(_) => debug!("sent ping to {}", self.addr),
            NetworkMessage::Pong(_) => debug!("sent pong to {}", self.addr),
            _ => (),
        }
        self.socket.send(message).await?;
        Ok(())
    }

    async fn send_version(&mut self) -> Result<(), Error> {
        self.send(NetworkMessage::Version(VersionMessage {
            version: PROTOCOL_VERSION,
            services: LOCAL_SERVICES,
            timestamp: now() as i64,
            receiver: Address::new(&self.addr, 0),
            sender: Address::new(
                &SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
                LOCAL_SERVICES,
            ),
            nonce: random(),
            user_agent: USER_AGENT.to_string(),
            start_height: 0,
            relay: true,
        }))
        .await?;
        Ok(())
    }

    async fn handshake(&mut self) -> Result<(), Error> {
        let mut ack = false;
        let mut version: Option<VersionMessage> = None;

        // version and verack should be first two messages we get
        for _ in 0..2 {
            let message = self
                .next()
                .timeout(Duration::from_secs(HANDSHAKE_TIMEOUT))
                .await?;
            if let Some(Ok(Message::Packet(message))) = message {
                match message {
                    NetworkMessage::Verack => {
                        debug!("received verack from {}", self.addr);
                        ack = true;
                    }
                    NetworkMessage::Version(version_message) => {
                        debug!("received version from {}", self.addr);
                        match version_message {
                            VersionMessage { version, .. } if version < MIN_VERSION => {
                                return Err(err_msg(
                                    "peer does not support required protocol version",
                                ))
                            }
                            VersionMessage { services, .. }
                                if services & Services::NETWORK as u64 == 0 =>
                            {
                                return Err(err_msg("peer does not support network services"));
                            }
                            VersionMessage { services, .. }
                                if services & Services::WITNESS as u64 == 0 =>
                            {
                                return Err(err_msg("peer does not support segregated witness"));
                            }
                            version_message => {
                                version = Some(version_message);
                                self.send(NetworkMessage::Verack).await?;
                            }
                        }
                    }
                    _ => (),
                }
            }
        }

        // can be chained when https://github.com/rust-lang/rust/issues/53667 merged

        if !ack {
            return Err(err_msg("hanshake failed"));
        }

        if let None = version {
            return Err(err_msg("hanshake failed"));
        }

        self.version = version;

        debug!("handshake complete with {}", self.addr);

        Ok(())
    }

    async fn handle_ping(&mut self, message: NetworkMessage) -> Result<(), Error> {
        if let NetworkMessage::Ping(nonce) = message {
            self.send(NetworkMessage::Pong(nonce)).await?;
        }
        Ok(())
    }

    fn handle_pong(&mut self, message: NetworkMessage) {
        match message {
            NetworkMessage::Pong(nonce) => {
                let now = now();

                match self.nonce {
                    Some(my_nonce) if nonce != my_nonce => {
                        debug!("Peer sent the wrong nonce {}", self.addr);
                    }
                    Some(_) => match self.last_ping {
                        Some(last_ping) if now >= last_ping => {
                            self.last_pong = Some(now);
                            if let None = self.min_ping {
                                self.min_ping = Some(now - last_ping);
                            }
                            self.min_ping = Some(min(self.min_ping.unwrap(), now - last_ping));
                            debug!("received successful pong {}", self.addr);
                        }
                        Some(_) => {
                            debug!("timing mismatch {}", self.addr);
                        }
                        None => {
                            self.last_pong = Some(now);
                            if let None = self.min_ping {
                                self.min_ping = Some(now);
                            }
                            self.min_ping = Some(min(self.min_ping.unwrap(), now));
                            debug!("received successful pong {}", self.addr);
                        }
                    },
                    None => {
                        debug!("peer sent an unsolicited pong, {}", self.addr);
                    }
                }
                self.nonce = None;
            }
            _ => (),
        }
    }

    async fn handle_heartbeat(&mut self) -> Result<(), Error> {
        if let Some(_) = &self.nonce {
            debug!("Peer has not responded to ping {}", self.addr);
            return Ok(()); // TODO: Decide whether to error or not
        }
        self.last_ping = Some(now());
        let nonce = random();
        self.nonce = Some(nonce);
        self.send(NetworkMessage::Ping(nonce)).await?;
        Ok(())
    }

    pub async fn start(&mut self) -> Result<(), failure::Error> {
        self.send_version().await?;
        self.handshake().await?;
        while let Some(result) = self.next().await {
            match result {
                // peer sent us a message to process
                Ok(Message::Packet(message)) => match message {
                    NetworkMessage::Version(_) => debug!("received version from {}", self.addr),
                    NetworkMessage::Verack => debug!("received verack from {}", self.addr),
                    NetworkMessage::Addr(_) => debug!("received addr from {}", self.addr),
                    NetworkMessage::Inv(_) => debug!("received inv from {}", self.addr),
                    NetworkMessage::Tx(_) => debug!("received tx from {}", self.addr),
                    NetworkMessage::Block(_) => debug!("received block from {}", self.addr),
                    NetworkMessage::Headers(_) => debug!("received headers from {}", self.addr),
                    NetworkMessage::SendHeaders => {
                        debug!("received sendheaders from {}", self.addr)
                    }
                    NetworkMessage::Ping(_) => {
                        debug!("received ping from {}", self.addr);
                        self.handle_ping(message).await?
                    }
                    NetworkMessage::Pong(_) => {
                        debug!("received pong from {}", self.addr);
                        self.handle_pong(message);
                    }
                    _ => (),
                },
                // a timer is ready
                Ok(Message::Heartbeat) => self.handle_heartbeat().await?,
                Err(e) => {
                    println!(
                        "an error occured while processing messages for {}; error = {}",
                        self.addr, e
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum Message {
    Packet(NetworkMessage),
    Heartbeat,
}

impl Stream for Peer {
    type Item = Result<Message, encode::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Poll::Ready(_) = self.ping_timer.poll_next_unpin(cx) {
            return Poll::Ready(Some(Ok(Message::Heartbeat)));
        }

        let result: Option<_> = futures::ready!(self.socket.poll_next_unpin(cx));

        Poll::Ready(match result {
            Some(Ok(message)) => Some(Ok(Message::Packet(message))),
            Some(Err(e)) => Some(Err(e)),
            None => None,
        })
    }
}
