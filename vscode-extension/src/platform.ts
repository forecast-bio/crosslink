import * as os from 'os';
import * as path from 'path';
import * as fs from 'fs';
import * as crypto from 'crypto';
import { chmodSync } from 'fs';

export type Platform = 'win32' | 'linux' | 'darwin';
export type Architecture = 'x64' | 'arm64';

export interface PlatformInfo {
    platform: Platform;
    arch: Architecture;
    binaryName: string;
    requiresChmod: boolean;
}

/**
 * Detects the current OS and architecture to select the correct binary.
 */
export function detectPlatform(): PlatformInfo {
    const platform = os.platform() as Platform;
    const arch = os.arch() as Architecture;

    // Validate supported platforms
    if (!['win32', 'linux', 'darwin'].includes(platform)) {
        throw new Error(`Unsupported platform: ${platform}`);
    }

    // Validate supported architectures
    if (!['x64', 'arm64'].includes(arch)) {
        throw new Error(`Unsupported architecture: ${arch}`);
    }

    // Determine binary name based on platform
    const binaryName = getBinaryName(platform, arch);
    const requiresChmod = platform !== 'win32';

    return {
        platform,
        arch,
        binaryName,
        requiresChmod,
    };
}

/**
 * Gets the binary filename for the given platform and architecture.
 */
function getBinaryName(platform: Platform, arch: Architecture): string {
    const archSuffix = arch === 'arm64' ? '-arm64' : '';

    switch (platform) {
        case 'win32':
            return `crosslink-win${archSuffix}.exe`;
        case 'linux':
            return `crosslink-linux${archSuffix}`;
        case 'darwin':
            return `crosslink-darwin${archSuffix}`;
        default:
            throw new Error(`Unknown platform: ${platform}`);
    }
}

/**
 * Resolves the path to the crosslink binary.
 *
 * @param extensionPath - The path to the extension directory
 * @param overridePath - Optional user-configured override path
 * @returns The absolute path to the binary
 */
export function resolveBinaryPath(extensionPath: string, overridePath?: string): string {
    // If user has configured an override, use it
    if (overridePath && overridePath.trim() !== '') {
        const resolved = path.resolve(overridePath);
        if (!fs.existsSync(resolved)) {
            throw new Error(`Configured binary not found: ${resolved}`);
        }
        return resolved;
    }

    // Use bundled binary
    const platformInfo = detectPlatform();
    const binaryPath = path.join(extensionPath, 'bin', platformInfo.binaryName);

    if (!fs.existsSync(binaryPath)) {
        throw new Error(
            `Bundled binary not found: ${binaryPath}\n` +
            `Expected binary for ${platformInfo.platform}/${platformInfo.arch}`
        );
    }

    return binaryPath;
}

/**
 * Ensures the binary has execute permissions on Unix systems.
 * Must be called before attempting to spawn the binary on Linux/macOS.
 *
 * @param binaryPath - Path to the binary
 */
export function ensureExecutable(binaryPath: string): void {
    const platformInfo = detectPlatform();

    if (!platformInfo.requiresChmod) {
        // Windows doesn't need chmod
        return;
    }

    try {
        // Check current permissions
        const stats = fs.statSync(binaryPath);
        const isExecutable = (stats.mode & fs.constants.S_IXUSR) !== 0;

        if (!isExecutable) {
            // Add execute permission for owner, group, and others
            // Mode 0o755: rwxr-xr-x
            chmodSync(binaryPath, 0o755);
        }
    } catch (error) {
        throw new Error(
            `Failed to set executable permissions on ${binaryPath}: ${error}`
        );
    }
}

/**
 * Verifies the SHA256 checksum of a bundled binary.
 *
 * If no .sha256 file exists (dev build or user-configured override),
 * verification is silently skipped.
 */
export function verifyBinaryChecksum(binaryPath: string): void {
    const checksumPath = binaryPath + '.sha256';
    if (!fs.existsSync(checksumPath)) {
        return;
    }
    const expected = fs.readFileSync(checksumPath, 'utf-8').trim();
    const hash = crypto.createHash('sha256');
    hash.update(fs.readFileSync(binaryPath));
    const actual = hash.digest('hex');
    if (actual !== expected) {
        throw new Error(
            `Binary integrity check failed for ${path.basename(binaryPath)}.\n` +
            `Expected: ${expected}\nActual:   ${actual}\n` +
            'The bundled binary may have been tampered with.'
        );
    }
}

/**
 * Validates that all required binaries are present for the current platform.
 * Useful for extension activation checks.
 */
export function validateBinaries(extensionPath: string): { valid: boolean; error?: string } {
    try {
        const binaryPath = resolveBinaryPath(extensionPath);
        ensureExecutable(binaryPath);
        verifyBinaryChecksum(binaryPath);
        return { valid: true };
    } catch (error) {
        return {
            valid: false,
            error: error instanceof Error ? error.message : String(error),
        };
    }
}
