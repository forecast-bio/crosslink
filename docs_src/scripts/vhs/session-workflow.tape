# Session Workflow — agent handoff across sessions
# Output: docs_src/assets/img/session-workflow.gif

Output ../assets/img/session-workflow.gif

Require crosslink

Set Shell "bash"
Set FontSize 14
Set Width 950
Set Height 600
Set Padding 20
Set Theme { "name": "crosslink", "black": "#1a1a2e", "red": "#f95838", "green": "#007c35", "yellow": "#f0a500", "blue": "#00a6db", "magenta": "#c77dff", "cyan": "#56cfe1", "white": "#e0e0e0", "brightBlack": "#555555", "brightRed": "#ff6b6b", "brightGreen": "#51cf66", "brightYellow": "#ffd43b", "brightBlue": "#74c0fc", "brightMagenta": "#d0a9f5", "brightCyan": "#99e9f2", "brightWhite": "#ffffff", "background": "#16161e", "foreground": "#e0e0e0", "selection": "#33467c", "cursor": "#f95838" }
Set TypingSpeed 35ms
Set WindowBar Colorful
Set WindowBarSize 40

# --- Agent Session 1 ---
Type "# ─── Agent Session 1: Start work ───"
Enter
Sleep 1s

Type "crosslink session start"
Enter
Sleep 2s

Type `crosslink quick "Implement dark mode" -p medium -l feature`
Enter
Sleep 1.5s

Type `crosslink session action "Added ThemeProvider wrapper to App.tsx"`
Enter
Sleep 0.8s

Type `crosslink session action "Dark palette complete, testing toggle"`
Enter
Sleep 0.8s

Type `crosslink session end --notes "Dark mode 80% done. Toggle works, need to persist preference in localStorage."`
Enter
Sleep 3s

# --- Agent Session 2: Picks up from handoff ---
Type "# ─── Agent Session 2: New agent reads handoff and continues ───"
Enter
Sleep 1s

Type "crosslink session start"
Enter
Sleep 3s

Type "crosslink session work 1"
Enter
Sleep 1.5s

Type `crosslink session action "Added localStorage persistence for theme preference"`
Enter
Sleep 0.8s

Type "crosslink close 1"
Enter
Sleep 1.5s

Type `crosslink session end --notes "Dark mode complete and shipped. Next: notifications (#2)."`
Enter
Sleep 3s
