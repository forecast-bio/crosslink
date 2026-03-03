import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';
import * as os from 'os';
import { execFileSync } from 'child_process';
import { DaemonManager } from './daemon';
import { validateBinaries, resolveBinaryPath, verifyBinaryChecksum } from './platform';

let daemonManager: DaemonManager | null = null;
let outputChannel: vscode.OutputChannel;
let statusBarItem: vscode.StatusBarItem;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
    outputChannel = vscode.window.createOutputChannel('Crosslink');
    context.subscriptions.push(outputChannel);

    // Create status bar item
    statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
    statusBarItem.command = 'crosslink.daemonStatus';
    context.subscriptions.push(statusBarItem);

    // Validate binaries are present
    const validation = validateBinaries(context.extensionPath);
    if (!validation.valid) {
        outputChannel.appendLine(`Binary validation failed: ${validation.error}`);
        vscode.window.showErrorMessage(
            `Crosslink: Binary not found for your platform. ${validation.error}`
        );
        return;
    }

    // Add binary directory to PATH for all terminals and child processes
    const binDir = path.join(context.extensionPath, 'bin');
    addToPath(context, binDir);
    outputChannel.appendLine(`Added to PATH: ${binDir}`);

    // Install binary to user's bin directory for shells that bypass VS Code's environment
    // (e.g., Git Bash spawned by Claude Code or other AI agents)
    try {
        const installed = await installToUserBin(context.extensionPath, outputChannel);
        if (installed) {
            outputChannel.appendLine(`Installed crosslink to user bin directory`);
        }
    } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        outputChannel.appendLine(`Note: Could not install to user bin: ${message}`);
    }

    // Get workspace folder
    const workspaceFolder = getWorkspaceFolder();
    if (!workspaceFolder) {
        outputChannel.appendLine('No workspace folder open');
        updateStatusBar(false);
        registerCommands(context);
        return;
    }

    // Get configuration
    const config = vscode.workspace.getConfiguration('crosslink');
    const overridePath = config.get<string>('binaryPath');
    const autoStart = config.get<boolean>('autoStartDaemon', true);
    const showOutput = config.get<boolean>('showOutputChannel', false);

    // Initialize daemon manager
    daemonManager = new DaemonManager({
        extensionPath: context.extensionPath,
        workspaceFolder,
        outputChannel,
        overrideBinaryPath: overridePath,
    });

    // Register commands
    registerCommands(context);

    // Auto-start daemon if configured and .crosslink exists
    if (autoStart && daemonManager.hasCrosslinkProject()) {
        try {
            await daemonManager.start();
            updateStatusBar(true);
            if (showOutput) {
                outputChannel.show();
            }
        } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            outputChannel.appendLine(`Failed to auto-start daemon: ${message}`);
            updateStatusBar(false);
        }
    } else {
        updateStatusBar(false);
    }

    // Watch for configuration changes
    context.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration((e) => {
            if (e.affectsConfiguration('crosslink')) {
                handleConfigChange();
            }
        })
    );

    // Check if Python is available for Claude Code hooks
    if (workspaceFolder) {
        checkPythonForHooks(workspaceFolder, outputChannel);
    }

    outputChannel.appendLine('Crosslink extension activated');
}

export function deactivate(): void {
    // Critical: Stop daemon when extension deactivates
    // This prevents zombie processes when VS Code closes
    if (daemonManager) {
        daemonManager.dispose();
        daemonManager = null;
    }
    outputChannel?.appendLine('Crosslink extension deactivated');
}

function registerCommands(context: vscode.ExtensionContext): void {
    const reg = (id: string, handler: () => Promise<void>) => {
        context.subscriptions.push(vscode.commands.registerCommand(id, handler));
    };

    // ── Init ──
    reg('crosslink.init', async () => {
        await executeCrosslinkCommand(['init'], 'Initializing crosslink project...');
    });

    // ── Session commands ──
    reg('crosslink.sessionStart', async () => {
        await executeCrosslinkCommand(['session', 'start'], 'Starting session...');
    });

    reg('crosslink.sessionEnd', async () => {
        const notes = await vscode.window.showInputBox({
            prompt: 'Enter handoff notes (optional)',
            placeHolder: 'What should the next session know?',
        });
        const args = ['session', 'end'];
        if (notes) {
            args.push('--notes', notes);
        }
        await executeCrosslinkCommand(args, 'Ending session...');
    });

    reg('crosslink.sessionStatus', async () => {
        await executeCrosslinkCommand(['session', 'status'], 'Getting session status...');
    });

    reg('crosslink.sessionWork', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID to work on',
            placeHolder: 'Enter issue number',
        });
        if (!id) { return; }
        await executeCrosslinkCommand(['session', 'work', id], `Setting working issue to #${id}...`);
    });

    reg('crosslink.sessionAction', async () => {
        const text = await vscode.window.showInputBox({
            prompt: 'Action breadcrumb',
            placeHolder: 'What are you working on right now?',
        });
        if (!text) { return; }
        await executeCrosslinkCommand(['session', 'action', text], 'Recording action...');
    });

    reg('crosslink.sessionLastHandoff', async () => {
        await executeCrosslinkCommand(['session', 'last-handoff'], 'Getting last handoff notes...');
    });

    // ── Daemon commands ──
    reg('crosslink.daemonStart', async () => {
        if (!daemonManager) {
            vscode.window.showErrorMessage('No workspace folder open');
            return;
        }
        try {
            await daemonManager.start();
            updateStatusBar(true);
            vscode.window.showInformationMessage('Crosslink daemon started');
        } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            vscode.window.showErrorMessage(`Failed to start daemon: ${message}`);
        }
    });

    reg('crosslink.daemonStop', async () => {
        if (!daemonManager) {
            vscode.window.showErrorMessage('No workspace folder open');
            return;
        }
        daemonManager.stop();
        updateStatusBar(false);
        vscode.window.showInformationMessage('Crosslink daemon stopped');
    });

    reg('crosslink.daemonStatus', async () => {
        if (!daemonManager) {
            vscode.window.showInformationMessage('Crosslink: No workspace open');
            return;
        }
        const running = daemonManager.isRunning();
        const pid = daemonManager.getPid();
        if (running && pid) {
            vscode.window.showInformationMessage(`Crosslink daemon running (PID: ${pid})`);
        } else {
            vscode.window.showInformationMessage('Crosslink daemon not running');
        }
    });

    // ── Issue listing & navigation ──
    reg('crosslink.listIssues', async () => {
        await executeCrosslinkCommand(['list'], 'Listing issues...');
    });

    reg('crosslink.readyIssues', async () => {
        await executeCrosslinkCommand(['ready'], 'Listing ready issues...');
    });

    reg('crosslink.blockedIssues', async () => {
        await executeCrosslinkCommand(['blocked'], 'Listing blocked issues...');
    });

    reg('crosslink.nextIssue', async () => {
        await executeCrosslinkCommand(['next'], 'Suggesting next issue...');
    });

    reg('crosslink.treeView', async () => {
        await executeCrosslinkCommand(['tree'], 'Showing issue tree...');
    });

    reg('crosslink.searchIssues', async () => {
        const query = await vscode.window.showInputBox({
            prompt: 'Search query',
            placeHolder: 'Enter search terms',
        });
        if (!query) { return; }
        await executeCrosslinkCommand(['search', query], `Searching for "${query}"...`);
    });

    // ── Issue creation ──
    reg('crosslink.createIssue', async () => {
        const title = await vscode.window.showInputBox({
            prompt: 'Issue title',
            placeHolder: 'Enter issue title',
        });
        if (!title) { return; }

        const priority = await vscode.window.showQuickPick(
            ['low', 'medium', 'high', 'critical'],
            { placeHolder: 'Select priority' }
        );

        const args = ['create', title];
        if (priority) {
            args.push('-p', priority);
        }

        await executeCrosslinkCommand(args, 'Creating issue...');
    });

    reg('crosslink.quickCreate', async () => {
        const title = await vscode.window.showInputBox({
            prompt: 'Issue title',
            placeHolder: 'Enter issue title',
        });
        if (!title) { return; }

        const priority = await vscode.window.showQuickPick(
            ['low', 'medium', 'high', 'critical'],
            { placeHolder: 'Select priority' }
        );

        const label = await vscode.window.showInputBox({
            prompt: 'Label (optional)',
            placeHolder: 'e.g. bug, feature, refactor',
        });

        const args = ['quick', title];
        if (priority) {
            args.push('-p', priority);
        }
        if (label) {
            args.push('-l', label);
        }

        await executeCrosslinkCommand(args, 'Quick creating issue...');
    });

    reg('crosslink.createWithTemplate', async () => {
        const title = await vscode.window.showInputBox({
            prompt: 'Issue title',
            placeHolder: 'Enter issue title',
        });
        if (!title) { return; }

        const template = await vscode.window.showQuickPick(
            ['bug', 'feature', 'refactor', 'research', 'audit', 'continuation', 'investigation'],
            { placeHolder: 'Select template' }
        );
        if (!template) { return; }

        await executeCrosslinkCommand(['create', title, '--template', template], `Creating ${template} issue...`);
    });

    reg('crosslink.createSubissue', async () => {
        const parentId = await vscode.window.showInputBox({
            prompt: 'Parent issue ID',
            placeHolder: 'Enter parent issue number',
        });
        if (!parentId) { return; }

        const title = await vscode.window.showInputBox({
            prompt: 'Subissue title',
            placeHolder: 'Enter subissue title',
        });
        if (!title) { return; }

        await executeCrosslinkCommand(['subissue', parentId, title], 'Creating subissue...');
    });

    // ── Issue details & modification ──
    reg('crosslink.showIssue', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID',
            placeHolder: 'Enter issue number',
        });
        if (!id) { return; }
        await executeCrosslinkCommand(['show', id], `Showing issue #${id}...`);
    });

    reg('crosslink.updateIssue', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID to update',
            placeHolder: 'Enter issue number',
        });
        if (!id) { return; }

        const field = await vscode.window.showQuickPick(
            [
                { label: 'Title', value: '--title' },
                { label: 'Description', value: '-d' },
                { label: 'Priority', value: '-p' },
            ],
            { placeHolder: 'What to update?' }
        );
        if (!field) { return; }

        let newValue: string | undefined;
        if (field.value === '-p') {
            newValue = await vscode.window.showQuickPick(
                ['low', 'medium', 'high', 'critical'],
                { placeHolder: 'Select new priority' }
            );
        } else {
            newValue = await vscode.window.showInputBox({
                prompt: `New ${field.label.toLowerCase()}`,
                placeHolder: `Enter new ${field.label.toLowerCase()}`,
            });
        }
        if (!newValue) { return; }

        await executeCrosslinkCommand(['update', id, field.value, newValue], `Updating issue #${id}...`);
    });

    reg('crosslink.closeIssue', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID to close',
            placeHolder: 'Enter issue number',
        });
        if (!id) { return; }
        await executeCrosslinkCommand(['close', id], `Closing issue #${id}...`);
    });

    reg('crosslink.closeAllIssues', async () => {
        const confirm = await vscode.window.showWarningMessage(
            'Close all open issues?',
            { modal: true },
            'Close All'
        );
        if (confirm !== 'Close All') { return; }
        await executeCrosslinkCommand(['close-all'], 'Closing all issues...');
    });

    reg('crosslink.reopenIssue', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID to reopen',
            placeHolder: 'Enter issue number',
        });
        if (!id) { return; }
        await executeCrosslinkCommand(['reopen', id], `Reopening issue #${id}...`);
    });

    reg('crosslink.deleteIssue', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID to delete',
            placeHolder: 'Enter issue number',
        });
        if (!id) { return; }

        const confirm = await vscode.window.showWarningMessage(
            `Delete issue #${id}? This cannot be undone.`,
            { modal: true },
            'Delete'
        );
        if (confirm !== 'Delete') { return; }

        await executeCrosslinkCommand(['delete', id, '-f'], `Deleting issue #${id}...`);
    });

    // ── Comments & labels ──
    reg('crosslink.addComment', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID',
            placeHolder: 'Enter issue number',
        });
        if (!id) { return; }

        const text = await vscode.window.showInputBox({
            prompt: 'Comment text',
            placeHolder: 'Enter your comment',
        });
        if (!text) { return; }

        await executeCrosslinkCommand(['comment', id, text], `Adding comment to #${id}...`);
    });

    reg('crosslink.addLabel', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID',
            placeHolder: 'Enter issue number',
        });
        if (!id) { return; }

        const label = await vscode.window.showInputBox({
            prompt: 'Label to add',
            placeHolder: 'e.g. bug, feature, refactor',
        });
        if (!label) { return; }

        await executeCrosslinkCommand(['label', id, label], `Adding label to #${id}...`);
    });

    reg('crosslink.removeLabel', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID',
            placeHolder: 'Enter issue number',
        });
        if (!id) { return; }

        const label = await vscode.window.showInputBox({
            prompt: 'Label to remove',
            placeHolder: 'Enter label name',
        });
        if (!label) { return; }

        await executeCrosslinkCommand(['unlabel', id, label], `Removing label from #${id}...`);
    });

    // ── Dependencies & relations ──
    reg('crosslink.blockIssue', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID that is blocked',
            placeHolder: 'Enter blocked issue number',
        });
        if (!id) { return; }

        const blockerId = await vscode.window.showInputBox({
            prompt: 'Blocker issue ID',
            placeHolder: 'Enter the issue that blocks it',
        });
        if (!blockerId) { return; }

        await executeCrosslinkCommand(['block', id, blockerId], `Blocking #${id} with #${blockerId}...`);
    });

    reg('crosslink.unblockIssue', async () => {
        const id = await vscode.window.showInputBox({
            prompt: 'Issue ID to unblock',
            placeHolder: 'Enter blocked issue number',
        });
        if (!id) { return; }

        const blockerId = await vscode.window.showInputBox({
            prompt: 'Blocker issue ID to remove',
            placeHolder: 'Enter the blocker issue number',
        });
        if (!blockerId) { return; }

        await executeCrosslinkCommand(['unblock', id, blockerId], `Unblocking #${id} from #${blockerId}...`);
    });

    reg('crosslink.relateIssues', async () => {
        const id1 = await vscode.window.showInputBox({
            prompt: 'First issue ID',
            placeHolder: 'Enter issue number',
        });
        if (!id1) { return; }

        const id2 = await vscode.window.showInputBox({
            prompt: 'Second issue ID',
            placeHolder: 'Enter related issue number',
        });
        if (!id2) { return; }

        await executeCrosslinkCommand(['relate', id1, id2], `Relating #${id1} and #${id2}...`);
    });

    reg('crosslink.unrelateIssues', async () => {
        const id1 = await vscode.window.showInputBox({
            prompt: 'First issue ID',
            placeHolder: 'Enter issue number',
        });
        if (!id1) { return; }

        const id2 = await vscode.window.showInputBox({
            prompt: 'Second issue ID',
            placeHolder: 'Enter related issue number',
        });
        if (!id2) { return; }

        await executeCrosslinkCommand(['unrelate', id1, id2], `Unrelating #${id1} and #${id2}...`);
    });
}

async function executeCrosslinkCommand(args: string[], statusMessage: string): Promise<void> {
    if (!daemonManager) {
        vscode.window.showErrorMessage('No workspace folder open');
        return;
    }

    try {
        await vscode.window.withProgress(
            {
                location: vscode.ProgressLocation.Notification,
                title: statusMessage,
                cancellable: false,
            },
            async () => {
                const output = await daemonManager!.executeCommand(args);
                if (output) {
                    outputChannel.appendLine(`$ crosslink ${args.join(' ')}`);
                    outputChannel.appendLine(output);
                    outputChannel.show(true);

                    // Show brief output in notification for short responses
                    const lines = output.split('\n');
                    if (lines.length <= 3) {
                        vscode.window.showInformationMessage(output);
                    }
                }
            }
        );
    } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        outputChannel.appendLine(`Error: ${message}`);
        vscode.window.showErrorMessage(`Crosslink: ${message}`);
    }
}

function getWorkspaceFolder(): string | undefined {
    const folders = vscode.workspace.workspaceFolders;
    if (!folders || folders.length === 0) {
        return undefined;
    }
    // Use first workspace folder
    return folders[0].uri.fsPath;
}

function updateStatusBar(running: boolean): void {
    if (running) {
        statusBarItem.text = '$(pulse) Crosslink';
        statusBarItem.tooltip = 'Crosslink daemon running (click for status)';
        statusBarItem.backgroundColor = undefined;
    } else {
        statusBarItem.text = '$(circle-slash) Crosslink';
        statusBarItem.tooltip = 'Crosslink daemon not running (click for status)';
        statusBarItem.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
    }
    statusBarItem.show();
}

function handleConfigChange(): void {
    const config = vscode.workspace.getConfiguration('crosslink');
    const newOverridePath = config.get<string>('binaryPath');

    // If binary path changed, we need to restart the daemon
    if (daemonManager?.isRunning()) {
        outputChannel.appendLine('Configuration changed, restarting daemon...');
        daemonManager.stop();

        const workspaceFolder = getWorkspaceFolder();
        if (workspaceFolder) {
            daemonManager = new DaemonManager({
                extensionPath: vscode.extensions.getExtension('crosslink.crosslink-issue-tracker')?.extensionPath || '',
                workspaceFolder,
                outputChannel,
                overrideBinaryPath: newOverridePath,
            });

            daemonManager.start().then(() => {
                updateStatusBar(true);
            }).catch((err) => {
                const message = err instanceof Error ? err.message : String(err);
                outputChannel.appendLine(`Failed to restart daemon: ${message}`);
                updateStatusBar(false);
            });
        }
    }
}

/**
 * Adds the crosslink binary directory to PATH for all VS Code terminals and tasks.
 * Uses VS Code's EnvironmentVariableCollection API which persists across sessions.
 * This allows `crosslink` commands to work in terminals and from AI agents.
 */
function addToPath(context: vscode.ExtensionContext, binDir: string): void {
    const envCollection = context.environmentVariableCollection;

    // Clear any stale entries first
    envCollection.delete('PATH');

    // Prepend our bin directory to PATH
    // This works cross-platform: Windows uses `;` separator, Unix uses `:`
    const separator = process.platform === 'win32' ? ';' : ':';
    envCollection.prepend('PATH', binDir + separator);

    // Make the modification persistent across VS Code restarts
    envCollection.persistent = true;

    // Also set for Windows Path (case variation)
    if (process.platform === 'win32') {
        envCollection.prepend('Path', binDir + separator);
    }
}

/**
 * Installs crosslink binary to user's personal bin directory.
 * This ensures the binary is available in shells that bypass VS Code's environment,
 * such as Git Bash spawned by Claude Code or other AI coding assistants.
 *
 * Target directories (in order of preference):
 * - Windows: %USERPROFILE%\bin, %USERPROFILE%\.local\bin
 * - Unix: ~/.local/bin, ~/bin
 */
async function installToUserBin(extensionPath: string, output: vscode.OutputChannel): Promise<boolean> {
    const homeDir = os.homedir();
    const isWindows = process.platform === 'win32';

    // Candidate directories - these are commonly in PATH
    const candidates = isWindows
        ? [
            path.join(homeDir, 'bin'),
            path.join(homeDir, '.local', 'bin'),
        ]
        : [
            path.join(homeDir, '.local', 'bin'),
            path.join(homeDir, 'bin'),
        ];

    // Find and verify source binary
    const sourceBinary = resolveBinaryPath(extensionPath);
    verifyBinaryChecksum(sourceBinary);
    const targetName = isWindows ? 'crosslink.exe' : 'crosslink';

    // Try each candidate directory
    for (const binDir of candidates) {
        // Check if directory exists (don't create it - user should have set it up)
        if (!fs.existsSync(binDir)) {
            continue;
        }

        const targetPath = path.join(binDir, targetName);

        // Always overwrite to ensure latest version
        if (fs.existsSync(targetPath)) {
            output.appendLine(`Updating crosslink at ${targetPath}`);
        }

        // Copy binary to user bin
        try {
            fs.copyFileSync(sourceBinary, targetPath);

            // Ensure executable on Unix
            if (!isWindows) {
                fs.chmodSync(targetPath, 0o755);
            }

            output.appendLine(`Installed crosslink to ${targetPath}`);
            return true;
        } catch (err) {
            output.appendLine(`Failed to copy to ${targetPath}: ${err}`);
            // Try next candidate
            continue;
        }
    }

    // No suitable bin directory found - try to create ~/.local/bin as fallback
    const fallbackDir = isWindows
        ? path.join(homeDir, 'bin')
        : path.join(homeDir, '.local', 'bin');

    try {
        fs.mkdirSync(fallbackDir, { recursive: true });
        const targetPath = path.join(fallbackDir, targetName);
        fs.copyFileSync(sourceBinary, targetPath);

        if (!isWindows) {
            fs.chmodSync(targetPath, 0o755);
        }

        output.appendLine(`Installed crosslink to ${targetPath}`);

        // Warn user they may need to add to PATH
        const pathHint = isWindows
            ? `Add ${fallbackDir} to your PATH environment variable`
            : `Add 'export PATH="$PATH:${fallbackDir}"' to your ~/.bashrc or ~/.zshrc`;

        vscode.window.showInformationMessage(
            `Crosslink installed to ${fallbackDir}. ${pathHint}`,
            'OK'
        );

        return true;
    } catch (err) {
        output.appendLine(`Failed to create fallback directory: ${err}`);
        return false;
    }
}

/**
 * Checks if Python is available when Claude Code hooks are configured.
 * Shows a warning if hooks exist but Python cannot be found.
 */
function checkPythonForHooks(workspaceFolder: string, output: vscode.OutputChannel): void {
    // Check if .claude/hooks directory exists with Python scripts
    const claudeHooksDir = path.join(workspaceFolder, '.claude', 'hooks');
    if (!fs.existsSync(claudeHooksDir)) {
        return; // No hooks directory, nothing to check
    }

    // Look for Python files in hooks directory
    let hasPythonHooks = false;
    try {
        const files = fs.readdirSync(claudeHooksDir);
        hasPythonHooks = files.some(f => f.endsWith('.py'));
    } catch {
        return; // Can't read directory, skip check
    }

    if (!hasPythonHooks) {
        return; // No Python hooks, nothing to check
    }

    // Check if Python is available
    const pythonCommands = process.platform === 'win32'
        ? ['python', 'python3', 'py']
        : ['python3', 'python'];

    let pythonFound = false;
    for (const cmd of pythonCommands) {
        try {
            execFileSync(cmd, ['--version'], {
                stdio: 'pipe',
                timeout: 5000
            });
            pythonFound = true;
            output.appendLine(`Python found: ${cmd}`);
            break;
        } catch {
            // Try next command
        }
    }

    if (!pythonFound) {
        output.appendLine('WARNING: Python not found but Claude Code hooks require it');
        vscode.window.showWarningMessage(
            'Crosslink: Python is required for Claude Code hooks but was not found. ' +
            'Install Python and ensure it\'s in your PATH for hooks to work.',
            'Install Python',
            'Dismiss'
        ).then(selection => {
            if (selection === 'Install Python') {
                vscode.env.openExternal(vscode.Uri.parse('https://www.python.org/downloads/'));
            }
        });
    }
}
