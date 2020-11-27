# rust-bitcoin-node

Bitcoin full node writen in rust using the family of rust-bitcoin libraries

# Supported Features

- Full block verification (script verification using [http://github.com/rust-bitcoin/rust-bitcoinconsensus](http://github.com/rust-bitcoin/rust-bitcoinconsensus))
- Compact blocks
- Mempool
- Transaction index
- Address index
- Compact block filter index and relay

# Todo

See [issues](https://github.com/jrawsthorne/rust-bitcoin-node/issues), mainly

- Mempool size limits & RBF
- Tor

Please don't only rely on this node for validating the chain.
