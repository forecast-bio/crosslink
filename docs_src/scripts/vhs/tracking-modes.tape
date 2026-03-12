# Tracking Modes — strict vs normal vs relaxed comparison
# Output: docs_src/assets/img/tracking-modes.gif

Output ../assets/img/tracking-modes.gif

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

# Agent checks its current enforcement level
Type "# Agent checks current tracking mode"
Enter
Type "crosslink config get tracking_mode"
Enter
Sleep 2s

# Show how modes affect agent behavior
Type "# Strict: agent must have an active issue before writing code"
Enter
Type `crosslink config set tracking_mode strict`
Enter
Sleep 2s

Type "# Normal: agent gets a reminder but can proceed"
Enter
Type `crosslink config set tracking_mode normal`
Enter
Sleep 2s

Type "# Relaxed: no enforcement, agent works freely"
Enter
Type `crosslink config set tracking_mode relaxed`
Enter
Sleep 2s

# Reset to recommended default
Type "# Reset to strict (recommended for multi-agent setups)"
Enter
Type `crosslink config set tracking_mode strict`
Enter
Sleep 3s
