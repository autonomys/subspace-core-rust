# Subspace Core Rust

A simple blockchain based on proofs-of-replication for the [Subspace Network](https://www.subspace.network), implemented in pure Rust.

Read the [specifications](/spec/overview.md) (wip) for more details.


### Install

If you have not previously installed the `gmp_mpfr_sys` crate, follow these [instructions](https://docs.rs/gmp-mpfr-sys/1.3.0/gmp_mpfr_sys/index.html#building-on-gnulinux).

RocksDB on Linux needs LLVM/Clang:
```bash
sudo apt-get install llvm clang
```

```
git clone https://github.com/subspace/subspace-core-rust.git
cd subspace-core-rust
cargo build --release
```

### Run Tests

`cargo test`

### Run Benches

`cargo bench`

Benches single block encode/decode time and full piece encode/decode time for each prime size.

### Run Node

`RUST_LOG=[level] cargo run -- run node-type`

`RUST_LOG=info cargo run -- run gateway`

`RUST_LOG=info cargo run -- run peer`

### Environment variables

#### SUBSPACE_DIR
`SUBSPACE_DIR` can be used to specify alternative default location for plot to be created in

Each node needs a different directory for testing -- example

`export SUBSPACE_DIR=~/Desktop/plots/subspace/peer0`

#### RUN_WS_RPC
`RUN_WS_RPC=1` will cause RPC server to be started on port `8880`.

https://www.npmjs.com/package/wscat can be used to test RPC:
```
wscat -c 127.0.0.1:8880
```

Supported RPC commands:
```
> {"method":"get_node_id","params":[],"id":1,"jsonrpc":"2.0"}
< {"jsonrpc":"2.0","result":"32d4bcea26b8b2fa9182f7b23abe6a0b53ce32684a8176b085da9d8cdea9bef3","id":1}
```
```
> {"method":"subscribe_blocks","id":2,"jsonrpc":"2.0"}
< {"jsonrpc":"2.0","result":1,"id":2}
```
```
> {"method":"unsubscribe_blocks","params":[1],"id":3,"jsonrpc":"2.0"}
< {"method":"unsubscribe_blocks","params":[1],"id":3,"jsonrpc":"2.0"}
```

### Cleanup
Remove `config.json`, `plot.bin`, `plot-map` and `plot-tags` at the location where client stores filed (printed during startup).

### Building and running Docker image:
```
docker build -t subspace-core-rust -f Dockerfile .
docker run --rm -it --init subspace-core-rust
```

### Building and running without Docker image:
To build an app run:
```
cargo build --release
```

Which will produce a binary that can be copied to one of the directories in PATH, for example:
```
sudo cp target/release/subspace /usr/local/bin/subspace
```

Then run a node:
```
subspace run gateway
```
Or as peer:
```
subspace run peer
```
Or as farmer:
```
subspace run farmer
```

### Status

1. ~~Sloth based proof-of-replication~~
2. ~~Disk plotter~~
3. ~~Evaluation Loop~~
4. ~~Ledger~~
5. ~~TCP Gossip Network~~
6. ~~Terminal Console~~
7. ~~Manages Forks~~
8. ~~Basic tx scheme~~
9. Erasure code state
10. Sync state chain

### Testing Only

Create a 2GB RAM Disk (mac)

`diskutil erasevolume HFS+ “RAMDisk” hdiutil attach -nomount ram://4194304`

