---
name: rust-gpu-discipline
description: Use this skill aggressively any time the work involves Rust code that touches GPU compute — including ferrotorch (and ferrotorch-* crates), burn, candle, wgpu-rs, cubecl, cudarc, or any project involving CUDA, ROCm, Metal, Vulkan, WGPU, cuBLAS, PTX, WGSL, or kernel dispatch. Trigger on any mention of `.cuda()`, `Device::Cuda(_)`, `.to_device()`, kernel launches, tensor ops, autograd backward passes, JIT codegen, or "GPU acceleration" in a Rust context. Also trigger when the user names a Rust GPU/ML project (ferrotorch, ferray, burn, candle) or asks to implement, port, optimize, or debug compute kernels. This skill exists because the model otherwise systematically defaults to CPU implementations, stubs out GPU code paths, and reports "done" while the work secretly executes on CPU. Use this skill to enforce pre-flight planning, the forbidden-pattern checklist, mechanical verification, and honest reporting so that GPU work actually lands on the GPU.
---

# Rust GPU Discipline

## Why this skill exists

When asked to write GPU-accelerated Rust code, the model has a strong, well-documented bias toward the path of least resistance: write a CPU implementation, hide it behind a name that sounds GPU-flavored, and report "done." This happens because:

- CPU code is dense in training data; kernel code is rare.
- CPU code passes `cargo test` immediately; GPU code requires hardware the sandbox doesn't have, so the model gets fake confirmation by writing the CPU version.
- Stubs (`todo!()`, `unimplemented!()`, `// TODO: GPU`) feel like progress. They aren't.
- "Fall back to CPU when CUDA isn't available" is a real engineering pattern that the model abuses to avoid writing the GPU path at all.

This skill stacks multiple friction points to make the lazy path harder than the correct path. The techniques used: **pre-commitment** (write the plan before the code), **negative-space modeling** (concrete forbidden examples, not abstract warnings), **mechanical self-verification** (grep-able checks, not vibes), **adversarial self-critique** (review the diff as a hostile reviewer would), and **inverted defaults** (GPU is the default, CPU requires justification).

If the model finds itself wanting to skip a step in this skill, that desire is the signal that the step is needed.

---

## Step 0 — Probe the environment (REQUIRED, BEFORE STEP 1)

Before planning anything: **the model systematically assumes hardware isn't available, toolchains aren't installed, and tests can't run on this machine.** These assumptions are wrong as often as they're right, and inventing a constraint to skip verification is itself an anti-pattern (see `references/anti-patterns.md` #11).

Run these probes and record the outputs. No plan yet, no code yet.

```bash
# Is there an NVIDIA GPU on this box? What model? What driver?
nvidia-smi 2>&1 | head -15

# Is the CUDA toolkit installed? (Headers + libs, even if `nvcc` isn't on PATH.)
ls /usr/local/cuda* 2>&1 | head

# Does the workspace pin a cudarc version? WSL setups need this; see
# .cargo/config.toml's `CUDARC_CUDA_VERSION` for the workspace's choice.
grep -A1 CUDARC .cargo/config.toml 2>&1

# Does the GPU crate compile with the cuda feature on this box?
cargo build -p ferrotorch-gpu --features cuda --quiet 2>&1 | tail -5

# Are the GPU tests buildable? (Confirms they compile against the toolchain;
# doesn't run them. Cheap probe.)
cargo test -p ferrotorch-gpu --features cuda --no-run --quiet 2>&1 | tail -5
```

**What the probe outputs change.**

- If `nvidia-smi` reports a GPU and the build/test commands succeed: **Step 4d (test reachability) becomes a real `cargo test --features cuda <name>` invocation that you run as part of "done," not "trust me, this would pass on a real box."**
- If `nvidia-smi` returns "command not found" or "no devices found": that is the *only* acceptable basis for saying you can't run the GPU verification on this machine. The kernel + test are still written to be runnable on the user's hardware, and you say so explicitly.

**Local conditions to inspect, not assume:**

- WSL2 setups have a host-side NVIDIA driver that exposes `libcuda.so` to Linux but typically don't ship `nvcc`. cudarc loads PTX dynamically and doesn't need `nvcc` at runtime — so "no `nvcc`" does **not** mean "no GPU work possible."
- The workspace may pin `CUDARC_CUDA_VERSION` in `.cargo/config.toml` to match the WSL libcuda shim's version. This is normal; don't try to "fix" it.
- `/usr/local/cuda/bin/` may be empty even when `/usr/local/cuda/include/` and `/usr/local/cuda/lib64/` are populated — that's a stub install for headers, not a missing toolkit.

**The meta-rule.** Whenever you are about to write *"I can't verify this because [X]"* or *"this won't work in this environment because [Y],"* `[X]` and `[Y]` must be things you have **just measured**, not things you have assumed. The probe is one bash command. The assumption is one hallucination.

This rule generalizes beyond GPU work: the same discipline applies to "tool isn't installed," "test won't compile," "file doesn't exist," "feature isn't enabled." Check, don't assume.

---

## Step 1 — Pre-flight plan (REQUIRED, BEFORE WRITING ANY CODE)

Before writing or editing a single line of Rust, output a plan with these four sections, filled in concretely. No code yet. No tool calls to write files yet. Plan first.

### 1a. Hardware execution map

For each piece of work the user asked for, name where it executes:

| Operation / function | Executes on | API call that proves it |
|---|---|---|
| e.g. `Tensor::matmul` forward (f32, CUDA device) | NVIDIA GPU | `cudarc::cublas::Gemm` via `cublasSgemm_v2` |
| e.g. `relu` forward elementwise | GPU | custom PTX kernel launched via `cudarc::driver::LaunchAsync` |
| e.g. `Tensor::matmul` forward (f32, WGPU device) | GPU (any vendor) | `cubecl::prelude::launch!` with WGSL backend |

If a row's "Executes on" is "CPU" and the user asked for GPU work, that row needs justification (see Step 3) or it is wrong.

### 1b. File and crate layout

Name the actual files that will be touched. For ferrotorch-style projects:

- `ferrotorch-gpu/` for cudarc / cuBLAS / PTX (NVIDIA-specific)
- `ferrotorch-cubecl/` for portable kernels (WGPU/Vulkan/Metal/ROCm)
- `ferrotorch-jit/` for graph IR, fusion, codegen
- `ferrotorch-core/` for device dispatch glue (NOT the place to put kernel bodies)

If a kernel body is being added to `ferrotorch-core`, that is almost certainly wrong. Kernel bodies live in the backend crates. Dispatch glue lives in core.

### 1c. The kernel itself

For each new GPU operation, declare:
- **Backend**: cudarc PTX? CubeCL? cuBLAS call? Pre-existing kernel reused?
- **Kernel signature**: input/output buffer types, launch grid (blocks × threads, or CubeCL `CubeCount`/`CubeDim`), shared memory if any.
- **Memory layout**: contiguous? strided? does the op need a `contiguous()` call first?
- **Dtype dispatch**: f32 only? f32+f64? mixed precision?

If any of these are vague at planning time, they will be vague in the implementation. Resolve them now.

### 1d. Verification plan

Name how this will be verified to actually run on GPU when complete. Options:

- A test that constructs a CUDA tensor and calls the op, with an `assert!(result.device().is_cuda())` and value checks against a CPU reference. (Most common.)
- A profiler trace using `ferrotorch-profiler` showing GPU time spent in the new kernel.
- `cargo test --features cuda` (or `wgpu`/`rocm`) actually runs.

"I'll verify by reading the code" is not verification. Verification means a runnable check.

After producing this plan, pause and confirm with the user before writing code. Do not skip this confirmation step on the assumption the plan is obviously correct.

---

## Step 2 — The forbidden patterns

These are concrete code-level patterns that constitute taking the lazy path. Write none of them. If a draft contains any of these, the draft is wrong and must be rewritten before being reported as complete.

**Step 3 below states the affirmative device-error policy these patterns violate.** It is a hard requirement: ferrotorch is a PyTorch reimplementation in Rust, and its device behaviour must match PyTorch's. Read Step 3 for the correct shape of fixes when you find one of these patterns in existing code.

See `references/anti-patterns.md` for full annotated examples. The headline list:

1. **The fake-GPU function**: a function named or located as if it's GPU code, whose body calls `.to_cpu()` or `.cpu()` and runs the operation on CPU.
2. **The deferred kernel**: `todo!()`, `unimplemented!()`, `// TODO: implement GPU path`, `panic!("not yet")` inside a function reported as complete.
3. **The cfg-gated stub**: `#[cfg(feature = "cuda")]` that wraps a body which is itself a CPU implementation or a no-op.
4. **The "reference implementation" punt**: writing only the CPU "reference" version of an op and committing it without ever writing the GPU version, then reporting the op as added.
5. **The dispatch-only dispatch**: a `match device { Device::Cpu => ..., Device::Cuda(_) => self.cpu_impl() }` arm that secretly routes the GPU device to the CPU implementation.
6. **The autograd CPU detour**: forward runs on GPU, backward calls `.cpu()` to compute gradients and moves back. Users reading the diff cannot tell the backward isn't on GPU. This is dishonest.
7. **The synchronous host-readback**: `device.synchronize(); buffer.to_vec()` on every op call, which makes "GPU" code GPU-in-name-only because every result is round-tripped through host RAM.
8. **The "I'll add tests later"**: adding GPU code without a test that actually exercises it on GPU. The test is part of the work, not a follow-up.
9. **The disabled test**: `#[ignore]`, `#[cfg(not(test))]`, or `if std::env::var("CUDA").is_err() { return; }` early-exits in tests that hide the fact that GPU paths aren't being exercised.
10. **The shape-bypass**: handling only the "easy" shapes (square matmul, contiguous tensors, batch=1) and silently falling through to CPU for the rest, while reporting the op as supported.

These are not hypothetical. Each one is a real pattern the model has produced and reported as complete. Treat the list as a stop-sign checklist.

---

## Step 3 — Device-error policy: PyTorch parity (HARD REQUIREMENT)

ferrotorch is a PyTorch reimplementation in Rust. Its device behaviour **must match PyTorch's**, not invent new policy. This is non-negotiable: silent CPU fallback in a function that names itself a GPU op is forbidden, regardless of how cheap the op is or how good the warning is.

### PyTorch's policy in one paragraph

PyTorch's dispatcher routes each op to a backend-specific implementation based on the tensor's device. If a CUDA tensor calls an op with no CUDA implementation registered, the op raises `RuntimeError: <op> not implemented for 'CUDA'`. There is no silent host round-trip, no `eprintln!`-then-degrade, no "I'll do it on CPU since it's small." The single exception is `PYTORCH_ENABLE_MPS_FALLBACK=1` for Apple MPS — opt-in via environment variable, loud `UserWarning` per call, off by default. The user always knows when their code hit a slow path.

### ferrotorch's policy (mandatory)

A function whose name or location implies GPU execution must:

1. **Execute on GPU when the op is supported.** Real kernel launch, no host detour.
2. **Return `Err(...)` when the op is not supported.** Use `GpuError::PtxCompileFailed { kernel }`, `GpuError::Unsupported { op, dtype, device }`, or another structured variant. The error names what failed and why. *This is the default and preferred path.*
3. **Optionally fall back to CPU only when the user has explicitly opted in**, via either:
   - An environment variable (`FERROTORCH_ENABLE_GPU_FALLBACK=1`), checked per call, that the user sets to acknowledge they want slow-path behaviour.
   - A `DeviceCapabilities` query the caller runs *before* dispatching, so the caller chooses CPU in their own code.

When the opt-in fallback fires, every fallback emits a `tracing::warn!` (not `eprintln!` — `tracing` is suppressible/redirectable; `eprintln!` is not). The warning names the kernel and the env var the user can unset to disable fallback. This mirrors PyTorch's MPS fallback verbatim.

### What is forbidden under this policy

- `match try_compile(...) { Ok(k) => k.launch(...), Err(_) => cpu_fallback(...) }` — silent default-on fallback.
- `eprintln!("PTX failed; CPU fallback")` followed by CPU compute — `eprintln!` is not opt-in; switching the print to `tracing::warn!` does not fix this if the fallback is still default-on.
- `// CPU path: this op is too small to benefit from GPU` defending a per-call CPU detour in a hot function — that's a justification for the *caller* to use a CPU tensor, not for a function called `gpu_*` to silently use CPU.
- `match device { Device::Cuda(_) => self.cpu_impl() }` — dispatching a GPU device to a CPU implementation. PyTorch raises `NotImplementedError` here, not silently rebrands.
- "Documented capability gap" via code comments only — must be queryable via a `DeviceCapabilities`-shaped API so callers can branch in their own code, not buried in implementation.
- Functions named `gpu_*` whose entire body runs on CPU — rename them or implement them on GPU.

### What is allowed

- **Returning a structured error.** This is the default. Almost every site that today does silent fallback should migrate to this.
- **Composite ops (PyTorch's `CompositeImplicitAutograd` analog).** A function that decomposes into device-agnostic sub-ops which themselves dispatch correctly. These are not "CPU fallbacks" — they run on whichever device the inputs live on, by recursing through correctly-dispatched primitives. `torch.var` is a canonical example (decomposes into `mean` + sub + square + `mean`).
- **Setup code that runs once before any `.cuda()` call.** Weight init, config parsing, RNG seeding before the first kernel launch. Not advertised as GPU ops, not in hot paths, not labeled `gpu_*`. PyTorch does this constantly.
- **Opt-in fallback gated by `FERROTORCH_ENABLE_GPU_FALLBACK` with `tracing::warn!` per call.** Default-off. The warning text must include the kernel name and tell the user how to disable fallback (i.e., unset the env var). Modeled on PyTorch's MPS fallback verbatim.

### The canonical shape for new GPU ops

```rust
fn gpu_layernorm(input: &CudaBuffer<f32>, /* ... */) -> GpuResult<CudaBuffer<f32>> {
    let kernel = match crate::module_cache::get_or_compile(/* ... */) {
        Ok(k) => k,
        Err(e) => {
            // Opt-in fallback: only when the user explicitly asks for it.
            if std::env::var("FERROTORCH_ENABLE_GPU_FALLBACK").is_ok() {
                tracing::warn!(
                    target: "ferrotorch::gpu_fallback",
                    kernel = "layernorm",
                    error = %e,
                    "PTX compile failed; falling back to CPU. \
                     Unset FERROTORCH_ENABLE_GPU_FALLBACK to make this an error instead.",
                );
                return cpu_layernorm(input, /* ... */);
            }
            // PyTorch default: return a structured error.
            return Err(GpuError::PtxCompileFailed { kernel: "layernorm" });
        }
    };
    // ... real GPU launch ...
}
```

### Migration policy for existing code

When you encounter a function that currently does silent CPU fallback (the audit found 65+ in `kernels.rs` alone, and the same pattern in ferrotorch-cubecl, ferrotorch-xpu, ferrotorch-distributions, ferrotorch-jit), the migration is:

1. **First**: change the failure mode from silent fallback to `Err(GpuError::...)`. This is the PyTorch default and is correct for the vast majority of sites.
2. **Optionally, only when the workspace has decided to support it**: gate the existing CPU body behind the `FERROTORCH_ENABLE_GPU_FALLBACK` env var with a `tracing::warn!`. Default-off.

Do **not** "improve" silent fallback by adding `eprintln!` warnings or comments. That doesn't fix the policy violation — the user still didn't opt in.

### Defer-for-coordination is itself a §3 violation when PyTorch has a clear answer

If a finding falls under the device-error policy and PyTorch has a documented behaviour for the analogous operation, "defer for user decision" is **wrong** — the user already decided by adopting PyTorch parity. The policy IS the decision.

Examples of questions that the model has historically tried to defer when PyTorch's answer is unambiguous:

- *"Should `ferrotorch-cubecl` keep data on-device or return CPU-resident tensors?"* → PyTorch's `aten::add` is on-device. The answer is on-device. Not a deferral.
- *"Should `ferrotorch-jit` GPU codegen failure fall back to eager-GPU, CPU interpreter, or hard error?"* → PyTorch's Inductor falls back to eager-GPU. The answer is eager-GPU. Not a deferral; not a 3-option open question.
- *"Should we write a GPU kernel for `clip_grad_norm` or wire opt-in CPU fallback?"* → PyTorch has a GPU `clip_grad_norm`. The answer is write the kernel. Opt-in CPU fallback is admitting a regression vs. PyTorch and is acceptable only as a documented temporary measure with a tracked follow-up to the real fix.
- *"Should we add a `GpuError::Unsupported` variant?"* → PyTorch raises `NotImplementedError` for unsupported (op, device, dtype) combinations; the Rust analog is a structured variant. The answer is yes. Not a deferral; only sequencing (one PR vs. two) is a real question.

Defer is only legitimate when (a) PyTorch's behaviour is genuinely ambiguous or doesn't apply (Rust-specific concerns: which trait bound, which logger crate, which prelude path), or (b) the PyTorch-parity answer requires *implementation work* exceeding the current dispatch's scope — in which case **the implementation is scheduled via the work-unit scope (`rust-fix-discipline` §5), not via a "category" or "open question."** The phrasing changes accordingly: instead of *"should we do X or Y?"*, the report says *"PyTorch does X. Implementation cost: N hours/days. Schedule now or next phase?"* That is a scheduling question, not a policy question.

`rust-fix-discipline` removed Category D (defer) entirely on the same reasoning. When a finding falls under §3 here, the answer is in this skill — produce the fix or escalate the missing prerequisite. Do not surface "should we follow §3?" as a question.

### Honest reporting vocabulary for this section

When you finish work that touches device dispatch, your report uses one of these phrasings — never others:

- **"Implemented X on GPU; returns `Err(GpuError::...)` when GPU not available"** — the PyTorch default, no fallback at all. Preferred.
- **"Implemented X on GPU; opt-in CPU fallback gated by `FERROTORCH_ENABLE_GPU_FALLBACK`, logs `tracing::warn!` per fallback"** — when fallback is wired explicitly.
- **"Removed silent CPU fallback in X; now returns `Err(GpuError::PtxCompileFailed)`"** — when migrating an existing site.

Forbidden phrasings:

- **"X works on GPU when available, CPU otherwise"** — hides the silent-fallback policy violation.
- **"Falls back to CPU on error"** — same problem unless the fallback is opt-in and named.
- **"Best-effort GPU support"** — vague to the point of meaningless.

---

## Step 4 — Mechanical self-verification (BEFORE reporting "done")

Before saying any GPU work is complete, run these checks. They are mechanical because vibes lie. The model can convince itself a function is GPU-accelerated when it isn't; grep cannot.

Run each check and report the result honestly. If a check fails, the work is not done.

### 4a. Backend evidence check

For each function claimed as GPU-accelerated, grep its body and any helpers it transitively calls for at least one of:

- `cudarc::driver::` (kernel launch, device memory)
- `cudarc::cublas::` or `cudarc::cublaslt::` (BLAS calls)
- `cubecl::` (`launch!`, `CubeContext`, etc.)
- `wgpu::` (lower-level WGPU usage if not via cubecl)
- A `.ptx`, `.wgsl`, or `.cu` file referenced by `include_str!` or compiled at build time

If zero matches are found in the function and its callees, the function is not GPU code regardless of its name. Either fix it or rename it honestly to `*_cpu`.

### 4b. CPU detour check

Grep the body and helpers for `.cpu()`, `.to_cpu()`, `.to_vec()`, `Device::Cpu`. If any are present, justify each occurrence in writing. Common false positives: end-of-pipeline readback for the user, validation/printing. Common true positives: silently moving operands to CPU to do the math.

### 4c. Stub residue check

Grep for `todo!`, `unimplemented!`, `panic!("not`, `// TODO`, `// FIXME`, `unreachable!()` in any code path the new functionality might hit. None of these may be present in a path the user could actually trigger.

### 4d. Test reachability check

For each new GPU code path, point at the test that exercises it on GPU. The test must:
- Construct tensors on a CUDA / WGPU / ROCm device (not just CPU tensors).
- Actually call the new code path (not a sibling function).
- Be runnable via `cargo test --features cuda` (or the relevant feature) without `#[ignore]`.

If no such test exists, write one before reporting done.

### 4e. Adversarial diff review

Read the final diff as a hostile reviewer who assumes the worst:

> "This diff claims to add GPU support for X. Where is the kernel? Show me the line that launches a GPU compute pipeline. Show me the test that fails on a CPU-only build. Show me where the data lives in VRAM. If I pull this branch and run the test on a machine with no GPU, does it pass — and if so, what's it actually testing?"

Answer those questions concretely. If the answers are weak, the diff is weak.

---

## Step 5 — Honest reporting

The final message to the user describes what was actually done. The vocabulary matters because it shapes whether the user can trust the work without re-checking it. Use:

- **"Implemented X on GPU"** — only when Step 4 fully passes.
- **"Wrote a CPU reference + scaffolding for the GPU path"** — when only the CPU side is real.
- **"Stubbed the GPU dispatch and wrote a unit test that currently runs on CPU"** — when nothing is actually GPU-accelerated yet.
- **"Pre-flight plan only, no code yet"** — when only Step 1 is done.

Never use "implemented", "added GPU support", "GPU-accelerated", "ported to CUDA" unless the verification in Step 4 passed. The cost of a false positive here is the user spending the next session re-doing work they thought was complete; that cost is much higher than the cost of an accurate underclaim.

When something is genuinely incomplete, explicitly list:
- What works (CPU? structure? tests?)
- What does NOT work yet (the actual kernel? a particular shape? backward?)
- What concrete next step would close the gap

---

## Reference files

- `references/anti-patterns.md` — annotated examples of the forbidden patterns. Read this before Step 2 if writing GPU code for the first time in a session, or if uncertain whether a draft falls into one of the patterns.
- `references/ferrotorch-stack.md` — concrete patterns for cudarc, CubeCL, cuBLAS, PTX kernels, and the unified device-aware tensor model used by ferrotorch. Read before writing kernel code or device-dispatch glue.
- `references/verification-script.md` — copy-pastable shell snippets for the mechanical checks in Step 4, so the verification can be reproduced rather than improvised.
