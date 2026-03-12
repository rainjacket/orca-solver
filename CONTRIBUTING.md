# Contributing to Orca

## Building

```bash
cargo build --release
```

## Testing

```bash
cargo test --all
```

All tests use a small built-in dictionary and don't require external files.

## Benchmarks

See the README for dictionary setup, then:

```bash
./bench.sh
```

## Code structure

- `crates/core/` -- Data structures: `BitSet`, `Dictionary`, `Grid`
- `crates/solver/` -- Search algorithm: constraint propagation, branching, parallelism
- `cli/` -- Command-line interface

## Pull requests

- Run `cargo fmt --all` and `cargo clippy --all-targets` before submitting
- Include tests for new functionality
- Keep changes focused -- one logical change per PR
