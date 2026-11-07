use std::path::Path;

fn main() {
    let root = Path::new("vendor/HLSLcc");
    let mut cpp = cc::Build::new();
    cpp.cpp(true)
        .std("c++11")
        .warnings(false)
        .include(root)
        .include(root.join("include"))
        .include(root.join("src/internal_includes"))
        .include(root.join("src/cbstring"))
        .include(root.join("src"))
        .file("shim/shim.cpp");
    for entry in std::fs::read_dir(root.join("src")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "cpp") {
            cpp.file(path);
        }
    }
    cpp.compile("hlslcc");

    let mut c = cc::Build::new();
    c.warnings(false).include(root.join("src/cbstring"));
    for entry in std::fs::read_dir(root.join("src/cbstring")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "c") {
            c.file(path);
        }
    }
    c.compile("hlslcc_bstring");

    println!("cargo:rerun-if-changed=shim/shim.cpp");
    println!("cargo:rerun-if-changed=vendor/HLSLcc");
}
