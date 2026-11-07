//! Decode a SMOL-V file to a SPIR-V binary: `decode <in.smolv> <out.spv>`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: decode <in.smolv> <out.spv>");
        std::process::exit(2);
    };
    let spirv = smolv::decode(&std::fs::read(input).unwrap()).unwrap();
    let bytes: Vec<u8> = spirv.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(output, bytes).unwrap();
}
