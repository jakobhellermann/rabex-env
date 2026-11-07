//! Round trip the SMOL-V stage chunks of Unity's Vulkan programs from the rabex-shader fixtures,
//! which Unity's `CompileVariant` wrote.

use std::path::Path;

const SPIRV_MAGIC: u32 = 0x0723_0203;

/// The non-empty stage chunks of a Unity Vulkan program: a flag word, then offset and size of 5 stages.
fn chunks(program: &[u8]) -> Vec<&[u8]> {
    let word =
        |i: usize| u32::from_le_bytes(program[i * 4..i * 4 + 4].try_into().unwrap()) as usize;
    (0..5)
        .map(|stage| (word(1 + 2 * stage), word(2 + 2 * stage)))
        .filter(|&(_, size)| size > 0)
        .map(|(offset, size)| &program[offset..offset + size])
        .collect()
}

#[test]
fn unity_vulkan_programs_round_trip() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rabex-shader/tests/fixtures");
    let mut checked = 0;
    for version in std::fs::read_dir(&root).unwrap() {
        let expected = version.unwrap().path().join("expected/shaders");
        // the 2020.2 fixtures have no Vulkan programs Unity wrote
        let Ok(entries) = std::fs::read_dir(&expected) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.contains(".Vulkan.") {
                continue;
            }
            let program = std::fs::read(&path).unwrap();
            if program.is_empty() {
                continue;
            }
            for (i, chunk) in chunks(&program).into_iter().enumerate() {
                let spirv =
                    smolv::decode(chunk).unwrap_or_else(|e| panic!("{name} chunk {i}: {e}"));
                assert_eq!(spirv[0], SPIRV_MAGIC, "{name} chunk {i}");
                assert_eq!(
                    spirv.len() * 4,
                    smolv::decoded_size(chunk).unwrap(),
                    "{name} chunk {i}"
                );
                let encoded =
                    smolv::encode(&spirv).unwrap_or_else(|e| panic!("{name} chunk {i}: {e}"));
                assert_eq!(encoded, chunk, "{name} chunk {i} differs after re-encoding");
                checked += 1;
            }
        }
    }
    // 2022 and 6000 carry expected Vulkan programs; the instancing shader adds four
    // stages per version
    assert_eq!(checked, 70, "stage chunks in the fixtures");
}
