test-unity2rust:
    cargo run --example unity2rust > examples/test_unity2rust.rs
    echo 'fn main() {}' >> examples/test_unity2rust.rs
    cargo c --example test_unity2rust

# Regenerate a generated module (default: rabex-shader's types.rs) from its typetrees.
# The generator writes its output to stdout; the module header above the first `#[derive]`
# line stays hand-written.
generate-types out="crates/rabex-shader/src/types.rs" config="examples/unity2rust.toml":
    #!/usr/bin/env bash
    set -euo pipefail
    tmp=$(mktemp)
    cargo run -q --example unity2rust -- "{{config}}" > "$tmp"
    python3 - "$tmp" "{{out}}" <<'PYEOF'
    import sys

    generated, out = sys.argv[1], sys.argv[2]
    body = open(generated).read().split("use rabex_env::unity::types::*;\n", 1)[1]
    body = body.replace("use indexmap::IndexMap;\n", "").lstrip("\n")
    header = open(out).read().split("#[derive", 1)[0]
    open(out, "w").write(header + body)
    PYEOF
    rm "$tmp"
    cargo fmt
    cargo c -q -p rabex-shader

# Byte-exact shader roundtrips over every game's real data; the fixtures cover only the
# shapes we thought of, the games cover the ones Unity actually writes.
# Downport every game's shaders to the 2020.2 form; the entries must roundtrip byte-exactly
downport-games:
    cargo run --release --example downport_roundtrip -- ~/.steamapps/Hollow\ Knight\ Silksong 2
    cargo run --release --example downport_roundtrip -- ~/.steamapps/Nine\ Sols 8

roundtrip-games:
    cargo run --release --example shader_roundtrip -- ~/.steamapps/Hollow\ Knight-1.5.78.11833 2
    cargo run --release --example shader_roundtrip -- ~/.steamapps/Nine\ Sols 8
    cargo run --release --example shader_roundtrip -- ~/.steamapps/Hollow\ Knight\ Silksong 2
    cargo run --release --example shader_roundtrip -- ~/.steamapps/Hollow\ Knight 2
