
use criterion::criterion_group;
use criterion::criterion_main;
use criterion::Criterion;
use rug::{rand::RandState, Integer};
use std::time::{SystemTime, UNIX_EPOCH};
use subspace_core_rust::sloth::Sloth;
use subspace_core_rust::crypto;
use subspace_core_rust::PIECE_SIZE;

pub fn criterion_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("Sloth");
    group.sample_size(10);

    for &prime_size in [256, 512, 1024, 2048, 4096_usize].iter() {

        let seed = Integer::from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis());
        let mut rand = RandState::new();
        rand.seed(&seed);
        let mut data = Integer::from(Integer::random_bits(prime_size as u32, &mut rand));
        let sloth = Sloth::init(prime_size);
        let mut encoding = sloth.sqrt_permutation(&data); 

        group.bench_function(format!("{} bits/encode-block", prime_size), |b| {
            b.iter(|| {
                data = sloth.sqrt_permutation(&data);
            })
        });

        group.bench_function(format!("{} bits/decode-block", prime_size), |b| {
            b.iter(|| {
                encoding = sloth.inverse_sqrt(&encoding);
            })
        });

        let iv = crypto::random_bytes_32();
        let expanded_iv = crypto::expand_iv(iv);
        let mut piece = crypto::generate_random_piece();
        let layers = PIECE_SIZE / sloth.block_size_bytes;
        let mut encoding = sloth.encode(&mut piece, expanded_iv, layers);

        group.bench_function(format!("{} bits/encode-piece", prime_size), |b| {
            b.iter(|| {
                piece = sloth.encode(&mut piece, expanded_iv, layers);
            })
        });

        group.bench_function(format!("{} bits/decode-piece", prime_size), |b| {
            b.iter(|| {
                encoding = sloth.decode(&mut encoding, expanded_iv, layers);
            })
        });
    }

    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);