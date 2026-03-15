# Maintenance — prune history and clean up stale agents
# Output: docs_src/assets/img/maintenance.gif

Output ../assets/img/maintenance.gif

Require crosslink

Set Shell "bash"
Set FontSize 14
Set Width 950
Set Height 550
Set Padding 20
Set Theme { "name": "crosslink", "black": "#1a1a2e", "red": "#f95838", "green": "#007c35", "yellow": "#f0a500", "blue": "#00a6db", "magenta": "#c77dff", "cyan": "#56cfe1", "white": "#e0e0e0", "brightBlack": "#555555", "brightRed": "#ff6b6b", "brightGreen": "#51cf66", "brightYellow": "#ffd43b", "brightBlue": "#74c0fc", "brightMagenta": "#d0a9f5", "brightCyan": "#99e9f2", "brightWhite": "#ffffff", "background": "#16161e", "foreground": "#e0e0e0", "selection": "#33467c", "cursor": "#f95838" }
Set TypingSpeed 35ms
Set WindowBar Colorful
Set WindowBarSize 40

# Agent runs maintenance after a long build cycle
Type "# Agent: running periodic maintenance"
Enter
Sleep 1s

# Check what would be pruned
Type "crosslink prune --hub --dry-run"
Enter
Sleep 2s

# Prune old hub history
Type "crosslink prune --hub --before 2026-02-01"
Enter
Sleep 2s

# Prune knowledge branch too
Type "crosslink prune --knowledge --before 2026-02-01"
Enter
Sleep 2s

# List active kickoff agents
Type "# Agent: checking for stale agents"
Enter
Type "crosslink kickoff list"
Enter
Sleep 2s

# Clean up stale worktrees and sessions
Type "crosslink kickoff cleanup"
Enter
Sleep 2.5s

# Verify integrity after cleanup
Type "crosslink integrity"
Enter
Sleep 3s
