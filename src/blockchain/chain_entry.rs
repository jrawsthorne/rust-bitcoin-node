use crate::protocol::{VERSIONBITS_TOP_BITS, VERSIONBITS_TOP_MASK};
use bitcoin::{
    consensus::encode, consensus::Decodable, consensus::Encodable, util::uint::Uint256, Block,
    BlockHash, BlockHeader, TxMerkleNode,
};

/// An entry in the blockchain.
/// Essentially a block header with its height in the blockchain specified
#[derive(Debug, Clone, Default, Copy, Eq, PartialEq)]
pub struct ChainEntry {
    pub hash: BlockHash,
    pub version: i32,
    pub prev_block: BlockHash,
    pub merkle_root: TxMerkleNode,
    pub time: u32,
    pub bits: u32,
    pub nonce: u32,
    /// Height of this entry in the blockchain
    pub height: u32,
    /// Cumulative mining work required to reach this block
    pub chainwork: Uint256,
    pub skip: BlockHash,
}

impl ChainEntry {
    /// Create a chain entry from a block header and previous chain entry (unless genesis)
    pub fn from_block_header(block_header: &BlockHeader, prev: Option<&Self>) -> Self {
        Self {
            hash: block_header.block_hash(),
            version: block_header.version,
            prev_block: block_header.prev_blockhash,
            merkle_root: block_header.merkle_root,
            time: block_header.time,
            bits: block_header.bits,
            nonce: block_header.nonce,
            height: match prev {
                Some(prev) => prev.height + 1,
                None => 0,
            },
            chainwork: match prev {
                Some(prev) => prev.chainwork + block_header.work(),
                None => block_header.work(),
            },
            skip: Default::default(),
        }
    }

    /// Create a chain entry from a block and previous chain entry (unless genesis)
    pub fn from_block(block: &Block, prev: Option<&Self>) -> Self {
        Self::from_block_header(&block.header, prev)
    }

    /// Whether the entry is for the genesis block
    pub fn is_genesis(&self) -> bool {
        self.height == 0
    }

    // does this entry signal for a certain version bit
    pub fn has_bit(&self, bit: u8) -> bool {
        (self.version & VERSIONBITS_TOP_MASK) == VERSIONBITS_TOP_BITS
            && (self.version & (1 << bit)) != 0
    }
}

impl From<&ChainEntry> for BlockHeader {
    fn from(entry: &ChainEntry) -> Self {
        Self {
            version: entry.version,
            prev_blockhash: entry.prev_block,
            merkle_root: entry.merkle_root,
            time: entry.time,
            bits: entry.bits,
            nonce: entry.nonce,
        }
    }
}

impl Encodable for ChainEntry {
    fn consensus_encode<W: std::io::Write>(&self, mut e: W) -> Result<usize, encode::Error> {
        Ok(self.hash.consensus_encode(&mut e)?
            + self.version.consensus_encode(&mut e)?
            + self.prev_block.consensus_encode(&mut e)?
            + self.merkle_root.consensus_encode(&mut e)?
            + self.time.consensus_encode(&mut e)?
            + self.bits.consensus_encode(&mut e)?
            + self.nonce.consensus_encode(&mut e)?
            + self.height.consensus_encode(&mut e)?
            + self.chainwork.consensus_encode(&mut e)?
            + self.skip.consensus_encode(&mut e)?)
    }
}

impl Decodable for ChainEntry {
    fn consensus_decode<D: std::io::Read>(mut d: D) -> Result<Self, encode::Error> {
        let hash = BlockHash::consensus_decode(&mut d)?;
        let version = i32::consensus_decode(&mut d)?;
        let prev_block = BlockHash::consensus_decode(&mut d)?;
        let merkle_root = TxMerkleNode::consensus_decode(&mut d)?;
        let time = u32::consensus_decode(&mut d)?;
        let bits = u32::consensus_decode(&mut d)?;
        let nonce = u32::consensus_decode(&mut d)?;
        let height = u32::consensus_decode(&mut d)?;
        let chainwork = Uint256::consensus_decode(&mut d)?;
        let skip = BlockHash::consensus_decode(&mut d)?;
        Ok(ChainEntry {
            hash,
            version,
            prev_block,
            merkle_root,
            time,
            bits,
            nonce,
            height,
            chainwork,
            skip,
        })
    }
}
