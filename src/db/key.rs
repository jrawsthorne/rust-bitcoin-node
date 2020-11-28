use bitcoin::consensus::Encodable;

pub trait DBKey: Encodable {
    fn col(&self) -> &'static str;
}
