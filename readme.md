# Subspace Core Rust

A simple blockchain based on proofs-of-replication for the [Subspace Network](https://www.subspace.network), implemented in pure Rust.


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

### Status

1. ~~Sloth based proof-of-replication~~
2. Disk plotter
3. Evaluation Loop
4. Ledger
5. TCP Gossip Network

