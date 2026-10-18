# Shader compilation cache

Repeated loads of an identical GLSL shader reuse its compiled SPIR-V within
the running process. Fragment shaders (generators, filters and transitions)
and compute shaders use the same cache with distinct stage keys.

This adapts the compiler-cache portion of [Varda f0f7279](https://github.com/im-knots/varda/commit/f0f727901e983b94a49832ff443e79c2ea41f570).
The public compiler functions, compiler settings and returned `Vec<u32>` remain
unchanged. GPU pipeline construction still runs for each instance. Background
effect loading is a separate change.

## Identity and lifetime

Entries compare the complete source, shader name and stage. Keeping full
inputs makes hash collisions harmless. Keeping the name accounts for shaderc's
source-name input. Compiler options and the `main` entry point are fixed; if
they become configurable, their values must also enter the key.

Only successful compilations are cached. An edited source gets a new entry;
invalid edits still report compiler errors with the current shader name.
Compiler warnings are logged on a cache miss. Each caller receives an owned
copy of the words, so modifying one result cannot change future loads.

The cache holds at most 512 entries, following Varda's policy. Inserting a new
entry when full clears the previous entries. A hit does not clear the cache.
The bound is on entry count; memory use depends on source and SPIR-V sizes.
Entries are discarded when the process exits.

The mutex protects lookup and insertion, with compilation outside the lock.
Concurrent misses for the same inputs may compile twice. A poisoned cache
falls back to ordinary compilation, keeping the optimization optional.

## Validation

Compiler tests cover returned SPIR-V equality with uncached compilation,
independent result ownership, edited and invalid sources, shader stages,
names and diagnostics, eviction, concurrent loads and poisoned-cache fallback.
The existing shader tests also validate representative SPIR-V through Naga.

Run the compiler tests and the compilation benchmark on a supported build:

```sh
cargo test --lib internal::isf::compiler::tests
cargo bench --bench shader_compilation
```

The benchmark compares repeated loads and cold compilations for fragment and
compute shaders. Cold iterations prepare a distinct comment before timing,
so they miss the cache without changing the picture. These timings measure
GLSL-to-SPIR-V compilation and result copying, rather than whole effect-load
latency, pipeline construction, frame rate or disk startup.

For a before/after comparison, put the same benchmark file and Cargo target
registration on the uncached baseline. Keep Criterion's results directory:

```sh
cargo bench --bench shader_compilation -- --save-baseline before
cargo bench --bench shader_compilation -- --baseline before
```

Use identical profiles and toolchains and run with other builds idle.

## Windows observations (18 October 2026)

On a Ryzen 9 5950X with Rust 1.99.1, a sequential before/after run of the
Criterion target measured these central estimates:

| Compilation | Uncached baseline | With cache |
|---|---:|---:|
| Repeated fragment | 217.56 ms | 135.15 ns |
| Repeated compute | 217.50 ms | 163.11 ns |
| Cold fragment | 225.95 ms | 222.46 ms |
| Cold compute | 218.93 ms | 226.16 ms |

Criterion detected no statistically significant change for the cold cases.
These are small synthetic shaders. The target was compiled directly with
`rustc -O -C lto=thin -C codegen-units=1` against the repository's existing
release dependencies, using the same harness before and after, to isolate the
compiler from upstream's Windows build blockers. Each case used ten samples,
a one-second warmup and a three-second measurement target, with other builds
idle. This does not establish effect-load times for arbitrary shaders.

A separate headless Windows application smoke test added and removed the
`invert` master effect four times via HTTP. The first request took 283.5 ms;
the next three took 19.8, 20.5 and 20.8 ms. Those requests include frame/command
scheduling and GPU pipeline creation, so they measure more than the compiler.

The Windows integration build passed 87 UI and 73 GPU rendering tests,
including pipeline construction for every bundled shader. Its full library
run passed 3,316 tests, ignored four and reproduced two previously recorded
DeckLink level-conversion failures (`full_levels_span_the_range` and
`levels_change_with_the_next_picture`). The production library and benchmark
passed Clippy with warnings denied. Whole-repository formatting and all-target
Clippy remain blocked by existing formatting and `assert_is_empty` test lint
issues. The changed Rust files pass rustfmt.
