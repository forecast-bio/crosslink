# Multi-Agent Coordination — agent lock management demo
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

# Agent registers itself on startup
Type "# Agent alpha registers automatically on session start"
Enter
Type `crosslink agent init agent-alpha -d "Frontend specialist"`
Enter
Sleep 2s

# Agent checks what's available
Type "# Agent checks which issues are available"
Enter
Type "crosslink locks list"
Enter
Sleep 2s

# Agent picks an unblocked, unlocked issue
Type "crosslink ready"
Enter
Sleep 1.5s

# Agent claims work — lock auto-acquired
Type "# Agent claims issue #3 — lock acquired automatically"
Enter
Type "crosslink session work 3"
Enter
Sleep 2s

# Show lock state
Type "crosslink locks list"
Enter
Sleep 2s

# Agent syncs with other agents
Type "# Agent syncs coordination state with other agents"
Enter
Type "crosslink sync"
Enter
Sleep 2s

# Agent picks next available issue (skips locked ones)
Type "crosslink next"
Enter
Sleep 3s
