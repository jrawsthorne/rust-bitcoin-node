use crate::protocol::{VERSIONBITS_TOP_BITS, VERSIONBITS_TOP_MASK};
use bitcoin::{util::uint::Uint256, Block, BlockHash, BlockHeader, TxMerkleNode};

/// An entry in the blockchain.
/// Essentially a block header with its height in the blockchain specified
#[derive(Debug, Clone, Default, Copy)]
pub struct ChainEntry {
    pub hash: BlockHash,
    pub version: u32,
    pub prev_block: BlockHash,
    pub merkle_root: TxMerkleNode,
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

    pub fn to_header(&self) -> BlockHeader {
        BlockHeader {
            version: self.version,
            prev_blockhash: self.prev_block,
            merkle_root: self.merkle_root,
            time: self.time,
            bits: self.bits,
            nonce: self.nonce,
        }
    }

    // does this entry signal for a certain version bit
    pub fn has_bit(&self, bit: u8) -> bool {
        (self.version & VERSIONBITS_TOP_MASK) == VERSIONBITS_TOP_BITS
            && (self.version & (1 << bit)) != 0
    }
}
