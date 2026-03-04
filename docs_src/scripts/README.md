# Documentation Visual Asset Scripts

Scripts for generating screenshots, terminal GIFs, and diagrams for the crosslink docs site.

## Prerequisites

```bash
brew install vhs                      # Terminal GIF recorder
brew install homeport/tap/termshot    # Terminal screenshot renderer
```

## Usage

```bash
# Generate all assets
./docs_src/scripts/generate-all.sh

# Or run individual scripts
vhs docs_src/scripts/vhs/hero-demo.tape
vhs docs_src/scripts/vhs/quickstart.tape
# etc.
```

## Structure

```
scripts/
├── generate-all.sh          # Master script — runs everything
├── vhs/                     # VHS .tape files for terminal GIFs
│   ├── hero-demo.tape       # Homepage hero GIF
│   ├── quickstart.tape      # Quick start walkthrough
│   ├── session-workflow.tape # Session lifecycle demo
│   ├── kickoff.tape         # /kickoff flow demo
│   ├── multi-agent.tape     # Multi-agent coordination demo
│   └── tracking-modes.tape  # Tracking mode comparison
├── termshot/                # Termshot commands for static screenshots
│   └── generate.sh          # All termshot captures
└── mermaid/                 # Mermaid diagram sources (.mmd)
    ├── session-lifecycle.mmd
    ├── kickoff-flow.mmd
    ├── multi-agent-arch.mmd
    ├── hook-pipeline.mmd
    ├── hook-decision.mmd
    └── tracking-modes.mmd
```

## Output

All generated assets go to `docs_src/assets/img/`. The `.qmd` files can reference them as:

```markdown
![Description](../assets/img/filename.gif)
```

## Notes

- VHS requires a working terminal (not headless CI) for font rendering
- Mermaid diagrams are also embedded inline in `.qmd` files via Quarto's native mermaid support — the `.mmd` files here are for standalone SVG generation if preferred
- Termshot captures use mock data so they produce consistent output
