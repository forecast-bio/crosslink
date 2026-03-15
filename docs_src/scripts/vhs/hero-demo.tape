# Homepage hero GIF — agent-first crosslink workflow overview
# Output: docs_src/assets/img/hero-demo.gif

Output ../assets/img/hero-demo.gif

Require crosslink

Set Shell "bash"
Set FontSize 15
Set Width 900
Set Height 500
Set Padding 20
Set Theme { "name": "crosslink", "black": "#1a1a2e", "red": "#f95838", "green": "#007c35", "yellow": "#f0a500", "blue": "#00a6db", "magenta": "#c77dff", "cyan": "#56cfe1", "white": "#e0e0e0", "brightBlack": "#555555", "brightRed": "#ff6b6b", "brightGreen": "#51cf66", "brightYellow": "#ffd43b", "brightBlue": "#74c0fc", "brightMagenta": "#d0a9f5", "brightCyan": "#99e9f2", "brightWhite": "#ffffff", "background": "#16161e", "foreground": "#e0e0e0", "selection": "#33467c", "cursor": "#f95838" }
Set TypingSpeed 40ms
Set WindowBar Colorful
Set WindowBarSize 40

# You give a high-level instruction...
Type "# You say: 'Fix the auth token refresh bug'"
Enter
Sleep 2s

# The agent starts a session and picks up context
Type "# Agent picks up where the last one left off"
Enter
Type "crosslink session start"
Enter
Sleep 2s

# Agent rapidly creates and tracks the work
Type `crosslink quick "Fix auth token refresh" -p high -l bug`
Enter
Sleep 1s

Type `crosslink session action "Found root cause in refresh_token()"`
Enter
Sleep 0.8s

Type `crosslink session action "Patched 401 retry logic"`
Enter
Sleep 0.8s

Type `crosslink issue comment 1 "Token refresh now retries with backoff on 401"`
Enter
Sleep 1s

# Agent closes and hands off
Type "crosslink close 1"
Enter
Sleep 1s

Type `crosslink session end --notes "Fixed token refresh (#1). Dark mode is next."`
Enter
Sleep 3s
