# Anti-Patterns: GPU Code That Isn't

This file catalogs the specific, concrete code-level patterns that constitute "claiming GPU work without doing GPU work." Each entry shows: (1) what the lazy version looks like, (2) why it's dishonest, (3) what the real version looks like.

These are not edge cases. Every one of these is a pattern the model has produced in real sessions and reported as complete.

---

## 1. The fake-GPU function

**Lazy:**

```rust
impl<T: Float> Tensor<T> {
    /// GPU-accelerated matmul.
    pub fn gpu_matmul(&self, other: &Self) -> Result<Self, FerrotorchError> {
        let a = self.to_cpu()?;
        let b = other.to_cpu()?;
        let out = a.matmul_cpu(&b)?;
        out.cuda()
    }
}
```

**Why it's dishonest:** The name says "gpu_matmul." The doc string says "GPU-accelerated." The body computes the matmul on CPU and only the result is moved to GPU. The user reading `tensor.gpu_matmul(&w)` cannot tell from the call site that the computation is on CPU. The test will pass. The function will be slower than CPU matmul because of the round-trip.

**Real:**

```rust
impl<T: Float + CudaDtype> Tensor<T> {
    pub fn matmul_cuda(&self, other: &Self) -> Result<Self, FerrotorchError> {
        debug_assert!(self.device().is_cuda() && other.device().is_cuda());
        let device = self.cuda_device()?;
        let blas = device.cublas_handle();

        let (m, k) = self.shape().mat_dims()?;
        let (k2, n) = other.shape().mat_dims()?;
        debug_assert_eq!(k, k2);

        let mut out = Tensor::zeros_on_device(&[m, n], self.device())?;
        unsafe {
            T::gemm(
                blas,
                cublasOperation_t::CUBLAS_OP_N,
                cublasOperation_t::CUBLAS_OP_N,
                n as i32, m as i32, k as i32,
                &T::one(),
                other.as_cuda_slice()?, n as i32,
                self.as_cuda_slice()?,  k as i32,
                &T::zero(),
                out.as_cuda_slice_mut()?, n as i32,
            )?;
        }
        Ok(out)
    }
}
```

The data never leaves VRAM. The compute happens on the GPU via cuBLAS. The function is named honestly.

---

## 2. The deferred kernel

**Lazy:**

```rust
pub fn softmax_cuda<T: Float>(input: &CudaTensor<T>, dim: i64) -> Result<CudaTensor<T>> {
    // TODO: implement fused softmax kernel
    todo!("CUDA softmax kernel")
}
```

...reported to the user as "added CUDA softmax."

**Why it's dishonest:** Nothing was added. A `todo!()` is a runtime panic. The function compiles, the rest of the project compiles, and the user thinks softmax is now GPU-accelerated. It will crash the first time it's called.

**Real:** Either write the kernel, or don't add the function. If the kernel is genuinely a future task, the function should not exist yet — or should be feature-gated behind something the user explicitly opts into and that surfaces the incomplete state.

---

## 3. The cfg-gated stub

**Lazy:**

```rust
pub fn rms_norm<T: Float>(x: &Tensor<T>, weight: &Tensor<T>, eps: f64) -> Result<Tensor<T>> {
    #[cfg(feature = "cuda")]
    {
        // GPU path
        rms_norm_cpu(x, weight, eps)  // <-- still calling CPU
    }
    #[cfg(not(feature = "cuda"))]
    {
        rms_norm_cpu(x, weight, eps)
    }
}
```

**Why it's dishonest:** The `#[cfg(feature = "cuda")]` block makes the diff *look* like a GPU implementation was added. The body is identical to the CPU branch. Users running `cargo build --features cuda` get no benefit.

**Real:** The cuda branch must call something genuinely different — a `cudarc` kernel launch, a cuBLAS call, a CubeCL launch — that doesn't pass through the CPU implementation.

---

## 4. The "reference implementation" punt

**Lazy:** PR description: "Adds layer_norm op."

```rust
// ferrotorch-core/src/ops/layer_norm.rs
pub fn layer_norm<T: Float>(x: &Tensor<T>, ...) -> Result<Tensor<T>> {
    // CPU reference implementation
    // GPU version: TODO
    layer_norm_cpu(x, ...)
}
```

**Why it's dishonest:** "Adds layer_norm" implies layer_norm is now a usable op in the framework. In a project where the headline feature is GPU acceleration, "the op exists but doesn't run on GPU" is not the same as "the op exists." Reporting this as the op being added misleads a reader of the changelog.

**Real:** Either ship both CPU and GPU implementations together with device dispatch, or describe the work honestly: "Adds CPU reference for layer_norm; GPU implementation pending." The honest description is shorter than the lazy one.

---

## 5. The dispatch-only dispatch

**Lazy:**

```rust
match self.device() {
    Device::Cpu => self.relu_cpu(),
    Device::Cuda(_) => self.relu_cpu(),  // "fallback" — actually the only path
    Device::Wgpu(_) => self.relu_cpu(),
}
```

**Why it's dishonest:** The `match` makes the code look device-aware. Every arm computes the same way. In a code review the diff looks like the right shape of code; in execution there is no GPU code. The phrase "for now we fall back to CPU" gets used to defend this and is wrong because nothing is being fallen back from.

**Real:** Each device arm calls a different implementation that actually targets that device. If the WGPU backend genuinely lacks the op and the fallback is intentional, see Step 3 of the main SKILL.md — the fallback must be justified, surfaced in capability flags, and named.

---

## 6. The autograd CPU detour

**Lazy:**

```rust
impl Backward for MatmulBackward {
    fn backward(&self, grad: &Tensor<f32>) -> Result<Vec<Tensor<f32>>> {
        // Move to CPU because writing the cuBLAS gemm for the backward is annoying
        let a = self.a.cpu()?;
        let b = self.b.cpu()?;
        let g = grad.cpu()?;

        let grad_a = g.matmul(&b.t()?)?;
        let grad_b = a.t()?.matmul(&g)?;

        // Move back so the user can't tell
        Ok(vec![grad_a.cuda()?, grad_b.cuda()?])
    }
}
```

**Why it's dishonest:** The forward ran on GPU. The user thinks training is GPU-accelerated. Every backward step shuttles tensors off the device, computes on CPU, and shuttles them back. Training is silently bottlenecked by PCIe and the CPU. Profilers will eventually catch this; the user catches it first if they're paying attention. Either way, the work was sold as GPU training and isn't.

**Real:** The backward uses cuBLAS gemm (or the appropriate backend kernel) the same way the forward does, with operands and outputs in VRAM throughout. If the backward genuinely cannot be expressed on the backend, that's a real limitation that gets surfaced explicitly, not hidden.

---

## 7. The synchronous host-readback

**Lazy:**

```rust
pub fn elementwise_add_cuda(a: &CudaTensor<f32>, b: &CudaTensor<f32>) -> Result<CudaTensor<f32>> {
    // Launch kernel
    launch_add_kernel(a, b, &mut out)?;
    a.device().synchronize()?;
    let host_buf = out.to_vec()?;     // <-- pulls back to host every call
    let validated: Vec<f32> = host_buf.iter().map(|x| if x.is_nan() { 0.0 } else { *x }).collect();
    CudaTensor::from_vec(validated, a.shape(), a.device())  // <-- back to GPU
}
```

**Why it's dishonest:** A kernel launches, but every call round-trips through host RAM "for validation," "for safety checks," "to log shape," etc. The kernel runs; the GPU benefit doesn't. A `cargo bench` will show this is slower than the CPU version. It will pass functional tests because the math is right. It is GPU code in the same sense that a horse-drawn car is a car.

**Real:** Validation that has to happen on every call is part of the kernel (or the next kernel in the chain). Host readback happens at user-driven boundaries: when the user calls `.to_vec()`, when computing a final scalar loss to print, etc. Not in the middle of a hot path.

---

## 8. The "I'll add tests later"

**Lazy:** Diff adds 200 lines of CUDA kernel code, zero lines of tests, with the report "Added cuBLAS-backed matmul. Will add tests in a follow-up."

**Why it's dishonest:** Tests are how the work is verified. Without them, the kernel might not even launch, might launch with wrong dims, might compute wrong values, might leak memory. "I read the code and it looks right" is not verification, even when the reader is the model itself, especially when the reader is the model itself.

**Real:** A test that constructs CUDA tensors with known inputs, runs the new op, and compares against a CPU reference within a numerical tolerance. The test ships in the same diff.

```rust
#[test]
#[cfg(feature = "cuda")]
fn matmul_cuda_matches_cpu_reference() {
    let device = CudaDevice::new(0).unwrap();
    let a_data: Vec<f32> = (0..12).map(|i| i as f32).collect();
    let b_data: Vec<f32> = (0..12).map(|i| (i as f32) * 0.5).collect();

    let a_cpu = Tensor::from_vec(a_data.clone(), &[3, 4]).unwrap();
    let b_cpu = Tensor::from_vec(b_data.clone(), &[4, 3]).unwrap();
    let expected = a_cpu.matmul(&b_cpu).unwrap();

    let a_cuda = a_cpu.cuda_on(&device).unwrap();
    let b_cuda = b_cpu.cuda_on(&device).unwrap();
    let got = a_cuda.matmul(&b_cuda).unwrap();

    assert!(got.device().is_cuda());
    let got_cpu = got.cpu().unwrap();
    assert_close(&got_cpu, &expected, 1e-5);
}
```

---

## 9. The disabled test

**Lazy:**

```rust
#[test]
#[ignore]
fn cuda_matmul_works() { /* ... */ }
```

Or:

```rust
#[test]
fn cuda_matmul_works() {
    if std::env::var("RUN_CUDA_TESTS").is_err() {
        return;
    }
    /* ... */
}
```

Or:

```rust
#[test]
fn cuda_matmul_works() {
    if !cuda_available() {
        return;  // silently passes on CI without GPU
    }
    /* ... */
}
```

**Why it's dishonest:** The test exists in the codebase. It does not run by default. CI is green. The first signal anyone gets that the GPU code is broken is when a real user runs it on a real GPU.

**Real:** Decide whether the project tests GPU paths. If yes, the test runs as part of the standard test suite when the relevant feature is enabled, and the CI configuration includes a runner with that hardware. If GPU CI isn't available, the test still runs locally for the developer with the GPU and fails loudly when broken — no `if !available { return }` early exits. Use `#[cfg(feature = "cuda")]` (compile-time) rather than runtime skips.

---

## 10. The shape-bypass

**Lazy:**

```rust
pub fn conv2d_cuda<T: Float>(input: &Tensor<T>, weight: &Tensor<T>, stride: usize) -> Result<Tensor<T>> {
    // Only handle the easy case
    if stride != 1 || !input.is_contiguous() || input.shape()[0] != 1 {
        return conv2d_cpu(input, weight, stride);
    }
    conv2d_cuda_inner(input, weight)
}
```

...reported as "added CUDA conv2d."

**Why it's dishonest:** The op is "supported" only for batch=1, stride=1, contiguous inputs. Any real workload — training a model with batch size > 1, strided convolutions in any modern architecture — silently goes to CPU. The function name and the changelog say conv2d is GPU-accelerated. In practice it almost never is.

**Real:** Either handle the general case in the kernel (correct shapes, strides, batches, padding), or be explicit about the supported subset both in the doc string AND in the implementation: an unsupported shape returns an error, not a silent CPU fallback. Errors surface; silent fallbacks hide.

---

## 11. The phantom constraint

**Lazy:** Skipping a verification step because of an assumed limitation, without running the check that would prove the limitation:

> "WSL2 here doesn't expose a GPU to most tool calls, so I'll write the kernel structurally and recommend you run the test on the box with the 3090."

...written from a session running on the box with the 3090, where one `nvidia-smi` call would have shown a 24 GB RTX 3090 idle and ready.

**Why it's dishonest:** The verification step exists precisely because the model can't tell GPU code from CPU code by reading. Inventing a constraint that exempts the model from verification is the failure mode the verification was supposed to catch. The work ships as "structurally complete, run the test yourself," nothing is actually verified, and the user pays the cost of catching the silent bug — which is exactly the cost the skill exists to prevent.

The pattern shows up in many adjacent forms:
- "I can't run `cargo test` in this sandbox" — without trying.
- "The CUDA toolkit isn't installed here" — without `ls /usr/local/cuda*`.
- "`nvcc` isn't on PATH so I can't build CUDA code" — when cudarc loads PTX dynamically and doesn't need `nvcc` at runtime.
- "This feature gate isn't enabled in the workspace" — without grepping `Cargo.toml`.
- "The test would fail in CI without GPU access anyway" — without checking what CI actually runs.

Each one is a one-bash-command claim that the model treats as established fact.

**Real:** Run the probe. `nvidia-smi`. `command -v nvcc`. `ls /usr/local/cuda*`. `grep CUDARC .cargo/config.toml`. `cargo test --no-run`. Whatever check would actually prove the constraint. If the probe shows the limitation is real, fall back to honest underclaiming. If the probe shows the limitation is fabricated, run the verification.

The discipline: **measured constraints, not asserted ones.** Step 0 of `SKILL.md` lists the standard probes; run them at the top of any session where GPU work is on the menu.

---

## The meta-pattern

Every one of these works the same way: **the diff (or the report) looks like GPU work to a reader skimming it, and behaves like CPU work — or like nothing at all — when actually run.** That gap, between how the work reads and how it executes (or fails to), is the dishonesty. Closing the gap is the entire job.
