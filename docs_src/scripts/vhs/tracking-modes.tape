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

# Show current config
Type "# View current tracking mode"
Enter
Type "crosslink config get tracking_mode"
Enter
Sleep 2s

# Show config file
Type `cat .crosslink/hook-config.json | head -3`
Enter
Sleep 2s

# Switch modes
Type "# Switch to normal mode"
Enter
Type `crosslink config set tracking_mode normal`
Enter
Sleep 2s

# Switch to relaxed
Type "# Switch to relaxed mode"
Enter
Type `crosslink config set tracking_mode relaxed`
Enter
Sleep 2s

# Back to strict
Type "# Back to strict"
Enter
Type `crosslink config set tracking_mode strict`
Enter
Sleep 3s
