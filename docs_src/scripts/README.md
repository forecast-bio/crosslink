# Documentation Visual Asset Scripts

Scripts for generating terminal GIFs, diagrams, and visual assets for the crosslink docs site.

## Prerequisites

```bash
brew install vhs                      # Terminal GIF recorder
npm install -g @mermaid-js/mermaid-cli # Mermaid diagram renderer (optional)
```

## Usage

```bash
# Generate all assets (GIFs, Mermaid SVGs, Python-generated SVGs)
./docs_src/scripts/generate-all.sh

# Or run individual scripts
vhs docs_src/scripts/vhs/hero-demo.tape
vhs docs_src/scripts/vhs/quickstart.tape
# etc.
```

## Structure

```
docs_src/scripts/
├── generate-all.sh          # Master script — runs everything
├── README.md                # This file
├── vhs/                     # VHS .tape files for terminal GIFs
│   ├── hero-demo.tape       # Homepage hero GIF (agent-first workflow)
│   ├── quickstart.tape      # Quick start walkthrough
│   ├── session-workflow.tape # Agent handoff across sessions
│   ├── kickoff.tape         # Autonomous agent launch demo
│   ├── multi-agent.tape     # Multi-agent lock coordination demo
│   ├── tracking-modes.tape  # Tracking mode comparison
│   └── maintenance.tape     # Prune history and cleanup stale agents
├── termshot/                # Termshot commands for static screenshots
│   └── generate.sh          # All termshot captures
└── mermaid/                 # Mermaid diagram sources (.mmd)
    ├── session-lifecycle.mmd
    ├── kickoff-flow.mmd
    ├── multi-agent-arch.mmd
    ├── hook-pipeline.mmd
    ├── hook-decision.mmd
    └── tracking-modes.mmd

scripts/                     # Project-root SVG diagram generators
├── generate-banner.py       # Banner SVG for images/banner.svg
└── generate-diagram-kickoff.py # Kickoff flow SVG diagram
```

`generate-all.sh` also discovers and runs any `scripts/generate-diagram-*.py` and `scripts/generate-banner.py` scripts from the project root.

## Output

All generated assets go to `docs_src/assets/img/`. The `.qmd` files can reference them as:

```markdown
![Description](../assets/img/filename.gif)
```

## Notes

- VHS requires a working terminal (not headless CI) for font rendering
- Mermaid diagrams are also embedded inline in `.qmd` files via Quarto's native mermaid support — the `.mmd` files here are for standalone SVG generation if preferred
- Termshot captures use mock data so they produce consistent output
- All VHS tapes use agent-first framing to show how agents drive crosslink commands
