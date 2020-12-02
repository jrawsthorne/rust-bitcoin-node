FROM rust:1.48

# for building rocksdb
RUN apt-get update && apt-get install -y clang

WORKDIR /rust-bitcoin-node

COPY . .

RUN cargo install --path .

CMD ["rust-bitcoin-node"]

VOLUME [ "/rust-bitcoin-node/data" ]