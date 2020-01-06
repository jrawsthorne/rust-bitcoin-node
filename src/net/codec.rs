use {
    bitcoin::{
        consensus::encode,
        network::{
            constants::Network,
            message::{NetworkMessage, RawNetworkMessage},
        },
    },
    bytes::{BufMut, BytesMut},
    tokio::io,
    tokio_util::codec::{Decoder, Encoder},
};

pub struct BitcoinCodec {
    network: Network,
}

impl BitcoinCodec {
    pub fn new(network: Network) -> BitcoinCodec {
        BitcoinCodec { network }
    }
}

impl Default for BitcoinCodec {
    fn default() -> Self {
        Self {
            network: Network::Bitcoin,
        }
    }
}

impl Decoder for BitcoinCodec {
    type Item = NetworkMessage;
    type Error = encode::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match encode::deserialize_partial::<RawNetworkMessage>(&buf) {
            Err(encode::Error::Io(ref err)) if err.kind() == io::ErrorKind::UnexpectedEof => {
                // Need to receive more bytes before can read full message
                Ok(None)
            }
            Err(err) => Err(err),
            // We have successfully read from the buffer
            Ok((message, index)) => {
                let expected_network = self.network;
                let network = Network::from_magic(message.magic);
                match network {
                    Some(network) if network == expected_network => {
                        let _ = buf.split_to(index);
                        Ok(Some(message.payload))
                    }
                    Some(network) => Err(encode::Error::UnexpectedNetworkMagic {
                        expected: expected_network.magic(),
                        actual: network.magic(),
                    }),
                    None => Err(encode::Error::UnknownNetworkMagic(message.magic)),
                }
            }
        }
    }
}

impl Encoder for BitcoinCodec {
    type Item = NetworkMessage;
    type Error = encode::Error;

    fn encode(&mut self, message: Self::Item, buf: &mut BytesMut) -> Result<(), Self::Error> {
        let serialized: Vec<u8> = encode::serialize(&RawNetworkMessage {
            magic: self.network.magic(),
            payload: message,
        });

        match serialized.len() {
            len if buf.remaining_mut() < len => {
                buf.reserve(len);
                buf.put(&serialized[..]);
            }
            _ => buf.put(&serialized[..]),
        }

        Ok(())
    }
}
