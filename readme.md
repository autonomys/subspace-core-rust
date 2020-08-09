# Subspace Core Rust

A simple blockchain based on proofs-of-replication for the [Subspace Network](https://www.subspace.network), implemented in pure Rust.

Read the [specifications](/spec/overview.md) (wip) for more details.


### Install

If you have not previously installed the `gmp_mpfr_sys` crate, follow these [instructions](https://docs.rs/gmp-mpfr-sys/1.3.0/gmp_mpfr_sys/index.html#building-on-gnulinux).

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

`RUST_LOG=[level] cargo run [node-type] [optional-path]`

`RUST_LOG=info cargo run gateway`

`RUST_LOG=info cargo run peer`

### Environment variables
* `SUBSPACE_DIR` can be used to specify alternative default location for plot to be created in

### Cleanup
Remove `config.json`, `plot.bin` and `plot-map.bin` at the location where client stores filed (printed during startup).

### Status

1. ~~Sloth based proof-of-replication~~
2. ~~Disk plotter~~
3. ~~Evaluation Loop~~
4. ~~Ledger~~
5. ~~TCP Gossip Network~~
6. ~~Terminal Console~~
7. ~~Manages Forks~~
8. Basic tx scheme
9. Erasrue code state

### Testing Only

Create a 2GB RAM Disk (mac)

`diskutil erasevolume HFS+ “RAMDisk” hdiutil attach -nomount ram://4194304`

