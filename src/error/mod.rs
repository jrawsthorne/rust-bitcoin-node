use thiserror::Error;

#[derive(Error, Debug)]
pub enum TransactionVerificationError {
    #[error("input missing or spent")]
    InputsMissingOrSpent,
    #[error("input value out of range")]
    InputValuesOutOfRange,
    #[error("input value less than output value")]
    InBelowOut,
    #[error("fee out of range")]
    FeeOutOfRange,
    #[error("input from coinbase spent before 100 confirmations")]
    PrematureCoinbaseSpend,
    #[error("no inputs")]
    InputsEmpty,
    #[error("no outputs")]
    OutputsEmpty,
    #[error("transaction size too large")]
    Oversized,
    #[error("output value too large")]
    OutputTooLarge,
    #[error("output total value too large")]
    OutputTotalTooLarge,
    #[error("duplicate input")]
    DuplicateInput,
    #[error("bad coinbase length")]
    BadCoinbaseLength,
    #[error("null previous output")]
    NullPreviousOutput,
    #[error("non final")]
    NonFinal,
    #[error("bad sigops count")]
    BadSigops,

    #[error("invalid scripts")]
    InvalidScripts,
}

#[derive(Error, Debug)]
pub enum BlockVerificationError {
    #[error(transparent)]
    BadHeader(#[from] BlockHeaderVerificationError),
    #[error(transparent)]
    BadTransaction(#[from] TransactionVerificationError),
    #[error("no coinbase")]
    NoCoinbase,
    #[error("bad merkle root")]
    BadMerkleRoot,
    #[error("no segwit")]
    NoSegwit,
    #[error("non final transactions")]
    NonFinalTransactions,
    #[error("bad coinbase height")]
    BadCoinbaseHeight,
    #[error("bad witness merkle root")]
    BadWitnessCommitment,
    #[error("unexpected witness")]
    UnexpectedWitness,
    #[error("bad block weight")]
    BadBlockWeight,
    #[error("missing coinbase")]
    MissingCoinbase,
    #[error("bad length")]
    BadLength,
    #[error("multiple coinbase transactions")]
    MultipleCoinbase,
    #[error("bad sigops")]
    BadSigops,
    #[error("bad coinbase amount")]
    BadCoinbaseAmount,
}

#[derive(Error, Debug)]
pub enum BlockHeaderVerificationError {
    #[error("bad difficult bits")]
    BadDifficultyBits,
    #[error("time too old")]
    TimeTooOld,
    #[error("time too new")]
    TimeTooNew,
    #[error("version too old")]
    Obsolete,
    #[error("invalid proof of work")]
    InvalidPOW,
}

#[derive(Debug, Error)]
pub enum DBError {
    #[error(transparent)]
    RocksDBError(#[from] rocksdb::Error),
    #[error(transparent)]
    EncodeError(#[from] bitcoin::consensus::encode::Error),
    #[error("{0}")]
    Other(&'static str),
}
