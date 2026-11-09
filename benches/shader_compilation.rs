//! Compare first compilation with repeated loads of an identical shader.
//! Cold iterations change a comment, outside the timed section, to miss the
//! source cache without changing the shader's rendered result.
use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use varda::isf::{compile_glsl_compute_to_spirv, compile_glsl_to_spirv};

type Compile = fn(&str, &str) -> anyhow::Result<Vec<u32>>;

const FRAGMENT: &str = "#version 450
layout(location = 0) out vec4 color;
void main() { color = vec4(0.25, 0.5, 0.75, 1.0); }
";
const COMPUTE: &str = "#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0, rgba8) writeonly uniform image2D output_image;
void main() { imageStore(output_image, ivec2(gl_GlobalInvocationID.xy), vec4(1.0)); }
";

fn shader_compilation(c: &mut Criterion) {
    let mut group = c.benchmark_group("shader_compilation");
    for (stage, source, compile) in [
        ("fragment", FRAGMENT, compile_glsl_to_spirv as Compile),
        ("compute", COMPUTE, compile_glsl_compute_to_spirv as Compile),
    ] {
        compile(source, "cache-bench").expect("benchmark shader compiles");
        group.bench_function(format!("{stage}/repeated"), |b| {
            b.iter(|| compile(black_box(source), "cache-bench").expect("compiles"));
        });
        let mut revision = 0_u64;
        group.bench_function(format!("{stage}/cold"), |b| {
            b.iter_batched(
                || {
                    revision += 1;
                    format!("{source}\n// revision {revision}")
                },
                |source| compile(black_box(&source), "cache-bench").expect("compiles"),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, shader_compilation);
criterion_main!(benches);
