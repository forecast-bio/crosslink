# Verification Snippets

Concrete, runnable checks for Step 0 (environment probe) and Step 4 (work-is-done verification) of the main SKILL.md. The point of using shell commands rather than reading the code is that grep doesn't lie about what's there. The model can talk itself into believing a function is GPU-accelerated; `rg cudarc src/` answers the question definitively. The model can talk itself into believing the box has no GPU; `nvidia-smi` answers that question definitively too.

Run from the repo root. Adapt path globs to whatever crate or directory is in scope for the change.

## 0. Environment probe (run at session start)

Before any of the Step 4 checks below, before any Step 1 plan, run these to find out what the machine actually has. The cost is one second; the cost of skipping is shipping a fabricated constraint and treating it as fact.

```bash
echo "=== GPU presence ===" && nvidia-smi 2>&1 | head -10
echo "=== CUDA toolkit ===" && ls /usr/local/cuda* 2>&1 | head -3
echo "=== Workspace cudarc pin ===" && grep -A1 CUDARC .cargo/config.toml 2>&1
echo "=== ferrotorch-gpu builds ===" && cargo build -p ferrotorch-gpu --features cuda --quiet 2>&1 | tail -3
echo "=== ferrotorch-gpu tests buildable ===" && cargo test -p ferrotorch-gpu --features cuda --no-run --quiet 2>&1 | tail -3
```

If `nvidia-smi` reports a GPU and the build succeeds: Step 4d is a real `cargo test --features cuda <name>` invocation, run as part of "done." Not a structural argument; an actual command, with actual output reported.

If `nvidia-smi` is missing or reports no devices: only then is "I can't run the GPU verification on this machine" honest. Even then, the kernel + test still get written so the user can run them on their hardware — and the report says explicitly which test to run, on what command line.

The general rule: any sentence that starts with "I can't verify this because [X]" must have `[X]` in the output of one of the commands above (or an equivalent probe). If `[X]` is something assumed without checking, the sentence is the bug.

## 4a. Backend evidence check

For each function claimed as GPU-accelerated, verify it (or something it transitively calls) actually invokes a backend.

```bash
# Did the new code invoke any of the GPU backends?
rg --type rust 'cudarc::|cubecl::|wgpu::|cublas::|nvrtc::|launch_unchecked|load_ptx' \
   path/to/changed/files/

# Are there any embedded kernel sources?
rg --type rust 'include_str!\("[^"]+\.(ptx|wgsl|cu|hip)"\)' .

# For a specific function, follow the call graph:
rg --type rust 'fn matmul_cuda' --multiline -A 30
```

Zero hits in the new code means no GPU work happened, regardless of function names.

## 4b. CPU detour check

```bash
# Look for CPU readbacks inside changed files
rg --type rust '\.cpu\(\)|\.to_cpu\(\)|\.to_vec\(\)|Device::Cpu' \
   path/to/changed/files/

# More specifically, inside functions named like GPU code:
rg --type rust -B 2 -A 20 'fn .*_cuda\b|fn .*_gpu\b|fn .*_wgpu\b' \
   path/to/changed/files/ | rg -B 5 -A 5 '\.cpu\(\)|\.to_vec\(\)'
```

Each hit should be justifiable. Hits inside a `_cuda` / `_gpu` / `_wgpu` function are red flags.

## 4c. Stub residue check

```bash
# Any unfinished work in code paths claimed as complete?
rg --type rust 'todo!|unimplemented!|panic!\("not |// TODO|// FIXME|unreachable!\(\)' \
   path/to/changed/files/
```

If anything new in the diff matches, it isn't done.

## 4d. Test reachability check

```bash
# Find tests added in this change
rg --type rust '#\[test\]' path/to/changed/files/ -A 5

# Are any of them gated off?
rg --type rust '#\[ignore\]|#\[cfg\(not\(test' path/to/changed/files/

# Are tests checking the device of the output?
rg --type rust '\.device\(\)\.is_cuda\(\)|assert.*Cuda|assert.*device' \
   path/to/changed/files/

# Do the tests run via a feature flag, and does that flag actually compile in the kernel?
cargo test --features cuda --no-run 2>&1 | tail -20
```

A test that doesn't construct a CUDA tensor and call the new code path doesn't verify the new code path.

## 4e. Adversarial diff review

Read the diff. Answer these out loud, in writing:

1. Show the line that launches a GPU compute pipeline (kernel launch, cuBLAS call, CubeCL launch, WGPU dispatch).
2. Show the test that would fail on a CPU-only build with the relevant feature flag enabled.
3. Show where the operands and output live in VRAM throughout the operation.
4. If someone pulls this branch and runs `cargo test --features cuda` on a machine WITHOUT a GPU, what happens? If it passes, what was the test actually exercising?
5. If the kernel were silently replaced with an `unimplemented!()`, which test would catch it?

If the answers to any of these are weak ("it's implicit," "the test would probably catch it," "I think it runs on GPU because the function is named that way"), the diff is weak. The verification has not passed.

## Quick dashboard

A one-shot script to run everything for a change scoped to `ferrotorch-gpu/src`:

```bash
#!/usr/bin/env bash
set -e
SCOPE="${1:-ferrotorch-gpu/src}"

echo "=== Backend evidence (should be > 0 lines) ==="
rg --type rust 'cudarc::|cubecl::|wgpu::|cublas::|launch_unchecked|load_ptx' "$SCOPE" | wc -l

echo "=== CPU detours in GPU-named functions (should be 0 unless justified) ==="
rg --type rust -B 1 -A 30 'fn .*_cuda\b|fn .*_gpu\b|fn .*_wgpu\b' "$SCOPE" \
  | rg -c '\.cpu\(\)|\.to_vec\(\)' || echo 0

echo "=== Stub residue (should be 0) ==="
rg --type rust 'todo!|unimplemented!|// TODO|// FIXME|unreachable!\(\)' "$SCOPE" | wc -l

echo "=== Tests added (should be > 0) ==="
rg --type rust '#\[test\]' "$SCOPE" | wc -l

echo "=== Ignored tests (should be 0) ==="
rg --type rust '#\[ignore\]' "$SCOPE" | wc -l

echo "=== cargo build with cuda feature ==="
cargo build --features cuda 2>&1 | tail -5
```

Numbers don't lie. Read them, report them honestly, and don't claim "done" until they say so.
