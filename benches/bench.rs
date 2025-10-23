#[path = "../examples/utils/mod.rs"]
mod utils;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use rabex_env::Environment;

fn bundle_locations(c: &mut Criterion) {
    let env = utils::find_game("silksong").unwrap().unwrap();

    let mut group = c.benchmark_group("find addressables bundle locations");

    group.bench_function("fs", |b| {
        b.iter(|| env.addressables_bundles().collect::<Vec<_>>())
    });
}

fn addressables_info(c: &mut Criterion) {
    let env = utils::find_game("silksong").unwrap().unwrap();
    c.bench_function("compute addressables info", |b| {
        b.iter(|| {
            let env = Environment::new(&env.game_files, &env.tpk);
            black_box(env.addressables().unwrap().unwrap());
        });
    });
}

criterion_group!(benches, bundle_locations, addressables_info);
criterion_main!(benches);
