# Multi-Agent Coordination — lock management demo
# Output: docs_src/assets/img/multi-agent.gif

Output ../assets/img/multi-agent.gif

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

# Register an agent
Type "# Register this machine as an agent"
Enter
Type `crosslink agent init agent-alpha -d "Frontend specialist"`
Enter
Sleep 2s

# Check agent status
Type "crosslink agent status"
Enter
Sleep 2s

# List locks
Type "# See which issues are taken"
Enter
Type "crosslink locks list"
Enter
Sleep 2s

# Check a specific issue
Type "crosslink locks check 5"
Enter
Sleep 1.5s

# Start working — lock is auto-claimed
Type "# Start work — lock auto-claimed"
Enter
Type "crosslink session work 3"
Enter
Sleep 2s

# Show locks again
Type "crosslink locks list"
Enter
Sleep 2s

# Sync coordination state
Type "# Sync with other agents"
Enter
Type "crosslink sync"
Enter
Sleep 2s

# Next — skips locked issues
Type "crosslink next"
Enter
Sleep 3s
