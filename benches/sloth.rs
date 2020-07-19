use criterion::criterion_group;
use criterion::criterion_main;
use criterion::Criterion;
use rug::integer::Order;
use rug::{rand::RandState, Integer};
use std::time::{SystemTime, UNIX_EPOCH};
use subspace_core_rust::crypto;
use subspace_core_rust::sloth::Sloth;
use subspace_core_rust::PIECE_SIZE;

pub fn criterion_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("Sloth");
    group.sample_size(10);

    for &prime_size in [256, 512, 1024, 2048, 4096_usize].iter() {
        let seed = Integer::from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        );
        let mut rand = RandState::new();
        rand.seed(&seed);
        let mut data = Integer::from(Integer::random_bits(prime_size as u32, &mut rand));
        let sloth = Sloth::init(prime_size);

        group.bench_function(format!("{} bits/encode-block", prime_size), |b| {
            b.iter(|| {
                sloth.sqrt_permutation(&mut data).unwrap();
            })
        });

        group.bench_function(format!("{} bits/decode-block", prime_size), |b| {
            b.iter(|| {
                sloth.inverse_sqrt(&mut data);
            })
        });

        let iv = crypto::random_bytes_32();
        let expanded_iv = crypto::expand_iv(iv);
        let integer_expanded_iv = Integer::from_digits(&expanded_iv, Order::Lsf);
        let mut piece = crypto::generate_random_piece();
        let layers = PIECE_SIZE / sloth.block_size_bytes;

        group.bench_function(format!("{} bits/encode-piece", prime_size), |b| {
            b.iter(|| {
                sloth
                    .encode(&mut piece, &integer_expanded_iv, layers)
                    .unwrap();
            })
        });

        group.bench_function(format!("{} bits/decode-piece-single", prime_size), |b| {
            b.iter(|| {
                sloth.decode(&mut piece, expanded_iv, layers);
            })
        });

        group.bench_function(format!("{} bits/decode-piece-parallel", prime_size), |b| {
            b.iter(|| {
                sloth.decode_parallel(&mut piece, expanded_iv, layers);
            })
        });
    }

    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
