use criterion::{black_box, criterion_group, criterion_main, Criterion};
use orca_core::BitSet;

fn bench_and_with_100k(c: &mut Criterion) {
    let mut a = BitSet::new_all_set(100_000);
    let b = BitSet::new_all_set(100_000);
    c.bench_function("and_with 100K bits", |bencher| {
        bencher.iter(|| {
            // Reset a to all-set each iteration isn't needed for benchmarking throughput
            black_box(a.and_with(black_box(&b)));
        });
    });
}

fn bench_count_ones_100k(c: &mut Criterion) {
    let bs = BitSet::new_all_set(100_000);
    c.bench_function("count_ones 100K bits", |bencher| {
        bencher.iter(|| {
            black_box(bs.count_ones());
        });
    });
}

fn bench_iter_ones_sparse(c: &mut Criterion) {
    let mut bs = BitSet::new(100_000);
    for i in (0..100_000).step_by(1000) {
        bs.set(i);
    }
    c.bench_function("iter_ones 100 set bits in 100K", |bencher| {
        bencher.iter(|| {
            let sum: usize = black_box(&bs).iter_ones().sum();
            black_box(sum);
        });
    });
}

fn bench_has_intersection_100k(c: &mut Criterion) {
    let mut a = BitSet::new(100_000);
    let mut b = BitSet::new(100_000);
    a.set(99_999);
    b.set(99_999);
    c.bench_function("has_intersection 100K bits (worst case)", |bencher| {
        bencher.iter(|| {
            black_box(a.has_intersection(black_box(&b)));
        });
    });
}

criterion_group!(
    benches,
    bench_and_with_100k,
    bench_count_ones_100k,
    bench_iter_ones_sparse,
    bench_has_intersection_100k
);
criterion_main!(benches);
