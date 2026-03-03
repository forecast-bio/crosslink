#!/usr/bin/env node
/**
 * Build script for crosslink binaries.
 * Compiles Windows, Linux, and macOS binaries from Rust source and copies to bin/.
 * Uses Docker with macos-cross-compiler for cross-compilation to macOS.
 * Requires: Docker, WSL (for Linux on Windows)
 */

const { execSync } = require('child_process');
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const os = require('os');

const ROOT_DIR = path.resolve(__dirname, '..', '..');
const CROSSLINK_DIR = path.join(ROOT_DIR, 'crosslink');
const BIN_DIR = path.join(__dirname, '..', 'bin');

// Add common tool directories to PATH for child processes
const extraPaths = process.platform === 'win32'
    ? [
        path.join(os.homedir(), 'AppData', 'Local', 'Microsoft', 'WinGet', 'Links'),
        path.join(os.homedir(), '.cargo', 'bin'),
    ]
    : [
        path.join(os.homedir(), '.cargo', 'bin'),
        '/usr/local/bin',
    ];

const pathSeparator = process.platform === 'win32' ? ';' : ':';
const enhancedPath = [...extraPaths, process.env.PATH].join(pathSeparator);
const enhancedEnv = { ...process.env, PATH: enhancedPath };

// Ensure bin directory exists
if (!fs.existsSync(BIN_DIR)) {
    fs.mkdirSync(BIN_DIR, { recursive: true });
}

function run(cmd, opts = {}) {
    console.log(`> ${cmd}`);
    try {
        execSync(cmd, { stdio: 'inherit', env: enhancedEnv, ...opts });
        return true;
    } catch (error) {
        console.error(`Command failed: ${cmd}`);
        return false;
    }
}

function checkCommand(cmd) {
    try {
        // zig uses 'version' subcommand, others use '--version'
        const versionArg = cmd === 'zig' ? 'version' : '--version';
        execSync(`${cmd} ${versionArg}`, { stdio: 'pipe', env: enhancedEnv, shell: true });
        return true;
    } catch {
        return false;
    }
}

function writeChecksum(filePath) {
    const hash = crypto.createHash('sha256');
    hash.update(fs.readFileSync(filePath));
    const checksum = hash.digest('hex');
    fs.writeFileSync(filePath + '.sha256', checksum + '\n');
    console.log(`  SHA256: ${checksum}`);
}

function buildWindows() {
    console.log('\n=== Building Windows binary ===');
    console.log('Cleaning previous build...');
    run('cargo clean', { cwd: CROSSLINK_DIR });
    const success = run('cargo build --release', { cwd: CROSSLINK_DIR });
    if (success) {
        const src = path.join(CROSSLINK_DIR, 'target', 'release', 'crosslink.exe');
        const dest = path.join(BIN_DIR, 'crosslink-win.exe');
        if (fs.existsSync(src)) {
            fs.copyFileSync(src, dest);
            writeChecksum(dest);
            console.log(`Copied: ${dest}`);
            return true;
        }
    }
    return false;
}

function buildLinux() {
    console.log('\n=== Building Linux binary (musl static linking) ===');

    const MUSL_TARGET = 'x86_64-unknown-linux-musl';

    if (process.platform === 'win32') {
        // Clean and build via WSL with musl target for static linking
        console.log('Cleaning previous Linux build...');
        run(`wsl -d FedoraLinux-42 -- bash -c "source ~/.cargo/env && cd /mnt/c/Users/texas/crosslink/crosslink/crosslink && cargo clean --target ${MUSL_TARGET} 2>/dev/null || true"`);
        console.log('Ensuring musl target is installed...');
        run(`wsl -d FedoraLinux-42 -- bash -c "source ~/.cargo/env && rustup target add ${MUSL_TARGET}"`);
        const wslCmd = `wsl -d FedoraLinux-42 -- bash -c "source ~/.cargo/env && cd /mnt/c/Users/texas/crosslink/crosslink/crosslink && cargo build --release --target ${MUSL_TARGET}"`;
        const success = run(wslCmd);
        if (success) {
            const src = path.join(CROSSLINK_DIR, 'target', MUSL_TARGET, 'release', 'crosslink');
            const dest = path.join(BIN_DIR, 'crosslink-linux');
            if (fs.existsSync(src)) {
                fs.copyFileSync(src, dest);
                writeChecksum(dest);
                console.log(`Copied: ${dest}`);
                run('wsl -d FedoraLinux-42 -- bash -c "chmod +x /mnt/c/Users/texas/crosslink/crosslink/vscode-extension/bin/crosslink-linux"');
                return true;
            }
        }
        return false;
    } else {
        // Native Linux build with musl target for static linking
        console.log('Cleaning previous build...');
        run(`cargo clean --target ${MUSL_TARGET}`, { cwd: CROSSLINK_DIR });
        console.log('Ensuring musl target is installed...');
        run(`rustup target add ${MUSL_TARGET}`);
        const success = run(`cargo build --release --target ${MUSL_TARGET}`, { cwd: CROSSLINK_DIR });
        if (success) {
            const src = path.join(CROSSLINK_DIR, 'target', MUSL_TARGET, 'release', 'crosslink');
            const dest = path.join(BIN_DIR, 'crosslink-linux');
            if (fs.existsSync(src)) {
                fs.copyFileSync(src, dest);
                fs.chmodSync(dest, 0o755);
                writeChecksum(dest);
                console.log(`Copied: ${dest}`);
                return true;
            }
        }
        return false;
    }
}

function buildMacOS() {
    console.log('\n=== Building macOS binaries (via Docker cross-compilation) ===');

    // Check if Docker is available
    if (!checkCommand('docker')) {
        console.log('Docker not found. Install from: https://www.docker.com/');
        return false;
    }

    const DOCKER_IMAGE = 'ghcr.io/shepherdjerred/macos-cross-compiler:latest';

    // Convert Windows path to Docker-compatible path
    const dockerWorkspace = process.platform === 'win32'
        ? ROOT_DIR.replace(/\\/g, '/').replace(/^([A-Za-z]):/, (_, letter) => `/${letter.toLowerCase()}`)
        : ROOT_DIR;

    let x64Ok = false;
    let arm64Ok = false;

    // Clean macOS targets first
    console.log('\n--- Cleaning macOS targets ---');
    run(`docker run --platform=linux/amd64 -v "${dockerWorkspace}:/workspace" --rm ${DOCKER_IMAGE} bash -c "cd /workspace/crosslink && cargo clean --target aarch64-apple-darwin --target x86_64-apple-darwin 2>/dev/null || true"`);

    // Build for aarch64 (Apple Silicon M1/M2/M3)
    console.log('\n--- Building for aarch64-apple-darwin ---');
    const arm64Cmd = `docker run --platform=linux/amd64 -v "${dockerWorkspace}:/workspace" --rm ${DOCKER_IMAGE} bash -c "cd /workspace/crosslink && export CC=aarch64-apple-darwin24-gcc && export AR=aarch64-apple-darwin24-ar && export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=aarch64-apple-darwin24-gcc && cargo build --release --target aarch64-apple-darwin"`;
    const arm64Success = run(arm64Cmd);
    if (arm64Success) {
        const src = path.join(CROSSLINK_DIR, 'target', 'aarch64-apple-darwin', 'release', 'crosslink');
        const dest = path.join(BIN_DIR, 'crosslink-darwin-arm64');
        if (fs.existsSync(src)) {
            fs.copyFileSync(src, dest);
            writeChecksum(dest);
            console.log(`Copied: ${dest}`);
            arm64Ok = true;
        }
    }

    // Build for x86_64 (Intel Macs)
    console.log('\n--- Building for x86_64-apple-darwin ---');
    const x64Cmd = `docker run --platform=linux/amd64 -v "${dockerWorkspace}:/workspace" --rm ${DOCKER_IMAGE} bash -c "cd /workspace/crosslink && export CC=x86_64-apple-darwin24-gcc && export AR=x86_64-apple-darwin24-ar && export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER=x86_64-apple-darwin24-gcc && cargo build --release --target x86_64-apple-darwin"`;
    const x64Success = run(x64Cmd);
    if (x64Success) {
        const src = path.join(CROSSLINK_DIR, 'target', 'x86_64-apple-darwin', 'release', 'crosslink');
        const dest = path.join(BIN_DIR, 'crosslink-darwin');
        if (fs.existsSync(src)) {
            fs.copyFileSync(src, dest);
            writeChecksum(dest);
            console.log(`Copied: ${dest}`);
            x64Ok = true;
        }
    }

    return x64Ok || arm64Ok;
}

function main() {
    console.log('Building crosslink binaries from source...');
    console.log(`Crosslink source: ${CROSSLINK_DIR}`);
    console.log(`Output directory: ${BIN_DIR}`);

    let windowsOk = false;
    let linuxOk = false;
    let macosOk = false;

    if (process.platform === 'win32') {
        windowsOk = buildWindows();
        linuxOk = buildLinux();
        macosOk = buildMacOS();
    } else if (process.platform === 'linux') {
        linuxOk = buildLinux();
        macosOk = buildMacOS();
        console.log('\nNote: Cross-compiling for Windows not configured on Linux.');
    } else if (process.platform === 'darwin') {
        // Native macOS build
        console.log('\n=== Building macOS binary (native) ===');
        console.log('Cleaning previous build...');
        run('cargo clean', { cwd: CROSSLINK_DIR });
        const success = run('cargo build --release', { cwd: CROSSLINK_DIR });
        if (success) {
            const src = path.join(CROSSLINK_DIR, 'target', 'release', 'crosslink');
            const arch = process.arch === 'arm64' ? 'crosslink-darwin-arm64' : 'crosslink-darwin';
            const dest = path.join(BIN_DIR, arch);
            if (fs.existsSync(src)) {
                fs.copyFileSync(src, dest);
                fs.chmodSync(dest, 0o755);
                writeChecksum(dest);
                console.log(`Copied: ${dest}`);
                macosOk = true;
            }
        }
        console.log('\nNote: Cross-compiling for Windows/Linux not configured on macOS.');
    }

    console.log('\n=== Build Summary ===');
    console.log(`Windows: ${windowsOk ? '✓' : '✗'}`);
    console.log(`Linux:   ${linuxOk ? '✓' : '✗'}`);
    console.log(`macOS:   ${macosOk ? '✓' : '✗'}`);

    // List binaries in bin directory
    console.log('\n=== Binaries in bin/ ===');
    const files = fs.readdirSync(BIN_DIR);
    files.forEach(f => {
        const stat = fs.statSync(path.join(BIN_DIR, f));
        console.log(`  ${f} (${(stat.size / 1024 / 1024).toFixed(2)} MB)`);
    });

    if (!windowsOk && !linuxOk && !macosOk) {
        console.error('\nNo binaries were built successfully.');
        process.exit(1);
    }
}

main();
