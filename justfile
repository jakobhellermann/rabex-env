test-unity2rust:
    cargo run --example unity2rust > examples/test_unity2rust.rs
    echo 'fn main() {}' >> examples/test_unity2rust.rs
    cargo c --example test_unity2rust
