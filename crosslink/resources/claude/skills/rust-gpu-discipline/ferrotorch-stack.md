# Ferrotorch GPU Stack Reference

This is the concrete how-to for the GPU backends ferrotorch (and similar Rust ML projects) actually uses. Read this before writing kernel code or device-dispatch glue, especially if it's been a while since the last GPU session — the model's prior on these APIs drifts.

## Crate map

| Crate | Role | What goes here |
|---|---|---|
| `ferrotorch-core` | Tensor type, autograd, device dispatch glue | The `match device { ... }` arms that dispatch to backends. NOT kernel bodies. |
| `ferrotorch-gpu` | NVIDIA-specific backend | cudarc usage, cuBLAS calls, custom PTX kernels, CUDA caching allocator, MemoryGuard / pre-OOM hooks |
| `ferrotorch-cubecl` | Portable backend | CubeCL kernels that compile to PTX/WGSL/HIP |
| `ferrotorch-jit` | JIT codegen | Tracing, IR, fusion passes, codegen to PTX/WGSL |
| `ferrotorch-distributed` | Multi-GPU | DDP, allreduce — usually built ON TOP of the per-device backends |

If a kernel body lands in `ferrotorch-core`, that is almost certainly a layering mistake. Push it down to `ferrotorch-gpu` or `ferrotorch-cubecl`.

---

## The unified device-aware tensor

ferrotorch (unlike PyTorch's history) does not have separate `Tensor` and `CudaTensor` types. There is one `Tensor<T>` that carries its device internally. Operations dispatch on the device:

```rust
pub fn matmul(&self, other: &Self) -> Result<Self, FerrotorchError> {
    match (self.device(), other.device()) {
        (Device::Cpu, Device::Cpu) => self.matmul_cpu(other),
        (Device::Cuda(d1), Device::Cuda(d2)) if d1 == d2 => self.matmul_cuda(other),
        (Device::Wgpu(d1), Device::Wgpu(d2)) if d1 == d2 => self.matmul_wgpu(other),
        (a, b) => Err(FerrotorchError::DeviceMismatch { lhs: a.clone(), rhs: b.clone() }),
    }
}
```

**Consequences for the model writing GPU code:**

- The temptation is to write only the `Cpu => self.matmul_cpu(other)` arm and route the others to it "until the kernel lands." This is anti-pattern 5. Don't.
- Cross-device ops should error, not implicitly move data. Implicit moves hide perf bugs.
- The `_cuda` / `_wgpu` methods do NOT need to start with a CPU readback. The tensor is already on the device.

---

## NVIDIA path: cudarc + cuBLAS + PTX

`cudarc` is a thin, safe Rust binding to the CUDA driver API. It's how ferrotorch-gpu talks to NVIDIA hardware. Mental model: cudarc gives Rust types around CUDA primitives; you still write the CUDA in CUDA.

### Device handle

```rust
use std::sync::Arc;
use cudarc::driver::CudaDevice;

let device: Arc<CudaDevice> = CudaDevice::new(0)?;  // ordinal 0
```

The `Arc<CudaDevice>` is what every kernel launch and allocation needs. Pass it through, don't reconstruct.

### Allocating device memory

```rust
let host: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0];
let dev_buf: cudarc::driver::CudaSlice<f32> = device.htod_copy(host)?;  // host -> device
// or:
let dev_zeros = device.alloc_zeros::<f32>(1024)?;
```

`CudaSlice<T>` is the device-side buffer. It does NOT live on the CPU. Calling `.to_vec()` on it triggers a host readback (DtoH copy). Don't do this in hot paths (anti-pattern 7).

### cuBLAS (GEMM, GEMV, etc.)

For matmul, the right tool is cuBLAS, not a custom kernel:

```rust
use cudarc::cublas::{CudaBlas, Gemm, GemmConfig};
use cudarc::cublas::sys::cublasOperation_t;

let blas = CudaBlas::new(device.clone())?;

let cfg = GemmConfig {
    transa: cublasOperation_t::CUBLAS_OP_N,
    transb: cublasOperation_t::CUBLAS_OP_N,
    m: m as i32,
    n: n as i32,
    k: k as i32,
    alpha: 1.0f32,
    lda: m as i32,
    ldb: k as i32,
    beta: 0.0f32,
    ldc: m as i32,
};

unsafe { blas.gemm(cfg, &a_slice, &b_slice, &mut c_slice)?; }
```

Note column-major layout. cuBLAS is column-major; if your tensors are row-major (PyTorch convention), the standard trick is to compute `B^T A^T = (AB)^T` by swapping operands and transpose flags, which gives row-major output without an explicit transpose.

### Custom PTX kernels (elementwise, fused ops)

For ops that aren't GEMM-shaped — relu, softmax, layer-norm, fused chains — write or generate PTX, embed it via `include_str!` or compile-time codegen, load it, and launch:

```rust
use cudarc::driver::{LaunchAsync, LaunchConfig};

let ptx = cudarc::nvrtc::compile_ptx(KERNEL_SRC)?;  // or include_str!("relu.ptx")
device.load_ptx(ptx, "relu_module", &["relu_f32"])?;

let kernel = device.get_func("relu_module", "relu_f32").unwrap();

let cfg = LaunchConfig::for_num_elems(n as u32);
unsafe { kernel.launch(cfg, (&input, &mut output, n as i32))?; }
```

`LaunchConfig::for_num_elems` picks a sensible 1D grid; for 2D/3D ops set the grid manually.

### Checking work happened on GPU

After writing the kernel, before claiming done:

1. The kernel source string is in the binary (grep for the kernel name).
2. There is a `device.load_ptx(...)` for the module containing it.
3. There is a `kernel.launch(...)` reachable from the public op function.
4. There is no `.to_vec()` / `.cpu()` between the public function entry and the launch.
5. A test on a CUDA device gets the right numerical answer.

If any of those is missing, the op is not GPU-accelerated regardless of how the function is named.

---

## Portable path: CubeCL

CubeCL lets one kernel definition compile to multiple backends (PTX for NVIDIA, WGSL for WGPU, HIP for AMD). This is what `ferrotorch-cubecl` uses. Mental model: write the kernel in Rust-with-attributes, the framework lowers it.

### A kernel

```rust
use cubecl::prelude::*;

#[cube(launch_unchecked)]
fn relu_kernel<F: Float>(input: &Array<F>, output: &mut Array<F>) {
    if ABSOLUTE_POS < input.len() {
        let x = input[ABSOLUTE_POS];
        output[ABSOLUTE_POS] = if x > F::new(0.0) { x } else { F::new(0.0) };
    }
}
```

### Launching it

```rust
use cubecl::wgpu::WgpuRuntime;  // or CudaRuntime, HipRuntime

let client = WgpuRuntime::client(&device);
let n = input.len();

let cube_count = CubeCount::Static(((n + 255) / 256) as u32, 1, 1);
let cube_dim   = CubeDim::new(256, 1, 1);

unsafe {
    relu_kernel::launch_unchecked::<F32, WgpuRuntime>(
        &client,
        cube_count,
        cube_dim,
        ArrayArg::from_raw_parts(&input_handle, n, 1),
        ArrayArg::from_raw_parts(&output_handle, n, 1),
    );
}
```

The same `relu_kernel` definition launched with `CudaRuntime` runs on NVIDIA via PTX; with `WgpuRuntime` runs on AMD/Intel/Apple via WGSL+Vulkan/Metal; with `HipRuntime` runs on AMD natively.

### CubeCL gotchas

- `ABSOLUTE_POS` is the linearized thread index — convenient for elementwise ops, not for reductions or tiled algorithms.
- Reductions need `subcube_*` ops or shared memory via `SharedMemory<T>::new(size)`.
- The `#[cube]` macro is strict about what subset of Rust you can use inside kernels. No `Vec`, no heap, limited control flow. If the body wants to do something fancy, it probably won't compile.
- For GEMM specifically, prefer the prebuilt `cubecl-linalg` or backend-specific BLAS — don't roll your own unless you know what you're doing with tiling and shared memory.

---

## JIT and codegen (`ferrotorch-jit`)

The JIT crate traces a forward pass into an IR, runs optimizations (constant folding, DCE, fusion), and emits backend code. Three things to know:

### The IR is the level to fuse at

Fusing two elementwise ops into one kernel — `relu(linear(x))` becoming a single GPU launch instead of two — happens by composing IR nodes and emitting a single kernel for the fused subgraph. Don't try to fuse at the ferrotorch-core level by writing a `relu_after_linear` op; that doesn't compose. Add the fusion pass to `ferrotorch-jit`.

### Codegen targets must match

PTX codegen targets NVIDIA. WGSL codegen targets WGPU. If you add an IR node that one backend doesn't lower, the JIT must surface that as a capability error, not silently fall back to the eager interpreter on a different device.

### The eager path and the JIT path must agree numerically

A test that runs an op eagerly and via the JIT-compiled graph should produce the same result within float tolerance. Without this, JIT bugs are invisible until they corrupt training.

---

## Memory: caching allocator and MemoryGuard

ferrotorch-gpu has a caching allocator modeled on PyTorch's `CUDACachingAllocator` and a `MemoryGuard` system for pre-OOM hooks. Two practical implications:

- **Don't allocate inside hot loops if avoidable.** `safe_alloc_with_hooks::<f32>(n)` is correct; calling it 1000 times per training step is correct but slow. Reuse buffers.
- **OOM is not a panic.** The MemoryGuard's `OomPolicy` controls behavior. New code that allocates should go through `MemoryGuard::safe_alloc_with_hooks`, not raw `device.alloc_zeros`, when running under the budget system.

---

## Autograd on GPU

The forward op records itself in the autograd graph. The backward op runs when `.backward()` is called. **Both must run on the same device.** If the forward is on CUDA, the saved tensors are on CUDA, and the backward implementation must consume CUDA tensors and produce CUDA tensors — using cuBLAS, custom kernels, or CubeCL the same way the forward does.

The path of least resistance — CPU-ifying the saved tensors in the backward, computing on CPU, and moving back — is anti-pattern 6. It is silent, it kills training throughput, and it is the single most common dishonesty in "GPU autograd" PRs.

---

## Concretely: how to add a new GPU op end-to-end

1. **Decide the backend(s).** For an op that maps to GEMM/GEMV: cuBLAS on NVIDIA, CubeCL `linalg` elsewhere. For elementwise / fused: CubeCL probably handles all backends; PTX directly if you need NVIDIA-specific perf.
2. **Add the kernel** to the right backend crate (`ferrotorch-gpu` or `ferrotorch-cubecl`).
3. **Add the autograd op** in `ferrotorch-core` with a `Forward` and `Backward` whose implementations dispatch to the kernel for each device.
4. **Add the public Tensor method** that constructs the autograd op and calls forward.
5. **Add tests** with at least: CPU correctness, CUDA correctness vs CPU, gradient correctness via `gradcheck`-style finite-differences on CPU AND on CUDA (the gradients should match between devices).
6. **Run the verification protocol** from the main SKILL.md Step 4.
7. **Report honestly** (Step 5).

Skipping any of 1–5 and reporting the op as added is the anti-pattern. The shortest honest version of this work touches all of them.
