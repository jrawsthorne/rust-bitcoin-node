pub trait DBKey {
    fn encode(&self) -> Result<Vec<u8>, bitcoin::consensus::encode::Error>;
    fn col(&self) -> &'static str;
}
