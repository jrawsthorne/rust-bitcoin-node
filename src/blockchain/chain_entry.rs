use bitcoin::{
    hashes::sha256d::Hash as H256, util::uint::Uint256, BitcoinHash, Block, BlockHeader,
};

/// An entry in the blockchain.
/// Essentially a block header with its height in the blockchain specified
#[derive(Debug, Clone, Default)]
pub struct ChainEntry {
    pub hash: H256,
    pub version: u32,
    pub prev_block: H256,
    pub merkle_root: H256,
    pub time: u32,
    pub bits: u32,
    pub nonce: u32,
    /// Height of this entry in the blockchain
    pub height: u32,
    /// Cumulative mining work required to reach this block
    pub chainwork: Uint256,
}

impl ChainEntry {
    /// Create a chain entry from a block header and previous chain entry (unless genesis)
    pub fn from_block_header(block_header: &BlockHeader, prev: Option<&Self>) -> Self {
        Self {
            hash: block_header.bitcoin_hash(),
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
}
