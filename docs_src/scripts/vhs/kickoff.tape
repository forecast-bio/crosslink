# Kickoff — autonomous feature agent launch demo
# Output: docs_src/assets/img/kickoff.gif
#
# NOTE: This tape simulates the /kickoff output since it requires
# tmux and a real Claude session. The commands shown demonstrate
# the user-facing workflow.

Output ../assets/img/kickoff.gif

Set Shell "bash"
Set FontSize 14
Set Width 1000
Set Height 600
Set Padding 20
Set Theme { "name": "crosslink", "black": "#1a1a2e", "red": "#f95838", "green": "#007c35", "yellow": "#f0a500", "blue": "#00a6db", "magenta": "#c77dff", "cyan": "#56cfe1", "white": "#e0e0e0", "brightBlack": "#555555", "brightRed": "#ff6b6b", "brightGreen": "#51cf66", "brightYellow": "#ffd43b", "brightBlue": "#74c0fc", "brightMagenta": "#d0a9f5", "brightCyan": "#99e9f2", "brightWhite": "#ffffff", "background": "#16161e", "foreground": "#e0e0e0", "selection": "#33467c", "cursor": "#f95838" }
Set TypingSpeed 30ms
Set WindowBar Colorful
Set WindowBarSize 40

# Show the kickoff command
Type "# Launch an autonomous feature agent"
Enter
Sleep 1s

Type `echo '/kickoff "add batch retry logic" --verify ci'`
Enter
Sleep 2s

# Simulate the output
Type "echo ''"
Enter
Type `echo "Creating feature branch..."`
Enter
Sleep 1s
Type `echo "  Branch:   feature/add-batch-retry-logic"`
Enter
Type `echo "  Worktree: .worktrees/add-batch-retry-logic/"`
Enter
Type `echo "  Issue:    #42 — add batch retry logic"`
Enter
Sleep 1.5s
Type `echo ""`
Enter
Type `echo "Detecting project tools..."`
Enter
Type `echo "  Found: Cargo.toml → cargo test, cargo clippy, cargo fmt"`
Enter
Sleep 1.5s
Type `echo ""`
Enter
Type `echo "Writing agent prompt → KICKOFF.md"`
Enter
Type `echo "Launching tmux session → feat-add-batch-retry-logic"`
Enter
Sleep 2s
Type `echo ""`
Enter
Type `echo "Feature agent launched."`
Enter
Type `echo ""`
Enter
Type `echo "  Worktree: .worktrees/add-batch-retry-logic"`
Enter
Type `echo "  Branch:   feature/add-batch-retry-logic"`
Enter
Type `echo "  Session:  feat-add-batch-retry-logic"`
Enter
Type `echo ""`
Enter
Type `echo "  Approve trust:  tmux attach -t feat-add-batch-retry-logic"`
Enter
Type `echo "  Check status:   /check feat-add-batch-retry-logic"`
Enter
Sleep 4s

# Show checking on it later
Type ""
Enter
Type "# Later — check on the agent's progress"
Enter
Sleep 1s
Type `echo '/check feat-add-batch-retry-logic'`
Enter
Sleep 1s
Type `echo ""`
Enter
Type `echo "Session: feat-add-batch-retry-logic"`
Enter
Type `echo "Status:  ✓ DONE"`
Enter
Type `echo "Issue:   #42 — add batch retry logic"`
Enter
Type `echo "Commits: 3 (feat: add retry logic, test: retry tests, fix: edge case)"`
Enter
Type `echo "PR:      #87 (draft, CI passing)"`
Enter
Sleep 4s
