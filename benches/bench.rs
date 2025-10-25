#[path = "../examples/utils/mod.rs"]
mod utils;

use std::fs::File;
use std::hint::black_box;
use std::io::Cursor;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rabex::files::SerializedFile;
use rabex::files::bundlefile::{BundleFileReader, ExtractionConfig};
use rabex_env::Environment;

fn parse(c: &mut Criterion) {
    let env = utils::find_game("silksong").unwrap().unwrap();
    let unity_version = env.unity_version().unwrap();
    let aa = env.addressables().unwrap().unwrap();
    let build_folder = aa.build_folder();

    let bundles = [
        "tk2dcollections_assets_areacoral.bundle",
        "textures_assets_areaaqueductareaswamp.bundle",
        "scenes_scenes_scenes/song_17.bundle",
        "scenes_scenes_scenes/memory_red.bundle",
        "herodynamic_assets_all.bundle",
    ];

    let mut bundle_group = c.benchmark_group("parse BundleFile");
    for name in bundles {
        let path = env.game_files.game_dir.join(&build_folder).join(name);

        bundle_group.throughput(Throughput::Bytes(std::fs::metadata(&path).unwrap().len()));
        bundle_group.bench_with_input(BenchmarkId::from_parameter(name), &path, |b, path| {
            b.iter(|| {
                let file = File::open(path).unwrap();
                let data = unsafe { memmap2::Mmap::map(&file).unwrap() };
                let reader = Cursor::new(data);
                let bundle = BundleFileReader::from_reader(
                    reader,
                    &ExtractionConfig::new(None, Some(unity_version.clone())),
                )
                .unwrap();
                let file = bundle
                    .serialized_files()
                    .find(|x| !x.path.ends_with("sharedAssets"))
                    .unwrap();
                bundle.read_at_entry(file).unwrap()

                /*let file = File::open(path).unwrap();
                let reader = BufReader::new(file);

                let mut bundle = BundleFileReader::from_reader(
                    reader,
                    &ExtractionConfig::new(None, Some(unity_version.clone())),
                )
                .unwrap();

                let mut found = false;
                while let Some(mut file) = bundle.next() {
                    if (file.flags & 4) != 0 && !file.path.ends_with("sharedAssets") {
                        found = true;
                        let data = file.read().unwrap();
                        black_box(data);
                    }
                }
                assert!(found);*/
            });
        });
    }
    bundle_group.finish();

    let mut serialized_group = c.benchmark_group("parse SerializedFile");
    for name in bundles {
        let bundle = env.load_addressables_bundle(name).unwrap();
        let file = bundle
            .serialized_files()
            .find(|x| !x.path.ends_with("sharedAssets"))
            .unwrap();
        let data = bundle.read_at_entry(file).unwrap();
        serialized_group.throughput(Throughput::Bytes(data.len() as u64));

        serialized_group.bench_with_input(
            BenchmarkId::from_parameter(name),
            data.as_slice(),
            |b, data| {
                b.iter(|| SerializedFile::from_reader(&mut Cursor::new(data)).unwrap());
            },
        );
    }
}

fn bundle_locations(c: &mut Criterion) {
    let env = utils::find_game("silksong").unwrap().unwrap();

    c.bench_function("scan for bundle locations", |b| {
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

criterion_group!(benches, parse, bundle_locations, addressables_info);
criterion_main!(benches);
