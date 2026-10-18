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
