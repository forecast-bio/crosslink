# Forecast Visual Design System

Reference for the Forecast brand visual language as implemented in crosslink docs and reusable across projects.

## Brand Palette (from `_brand.yml`)

| Name   | Hex       | Usage                                    |
|--------|-----------|------------------------------------------|
| Red    | `#F95838` | Primary accent, CTAs, crosslink branding |
| Green  | `#007C35` | Success, agents, session markers         |
| Blue   | `#00A6DB` | Links, info, agent identity              |
| Yellow | `#FFCE02` | Warnings, highlights, knowledge          |
| Pink   | `#FFB6C6` | Soft backgrounds, decorative             |
| Grey   | `#ede2e4` | Confetti base (warm pinkish grey)        |
| Bg     | `#F9F4F5` | Page background                          |

## Typography

- **Headings (h1)**: Helvetica, bold
- **Subheadings (h2)**: Times New Roman, italic
- **Body text**: Helvetica
- **Code / slash-commands only**: IBM Plex Mono — never use mono for non-code text

## Shape Vocabulary

Shapes use only: ellipses, circles, rounded rectangles. No triangles or crescents.

All shapes use `mix-blend-mode: multiply` — no alpha transparency on shapes. This creates the layered, overlapping Forecast look where colors interact.

## Confetti Dots

Small colored circles scattered as decorative accents, always with `mix-blend-mode: multiply`.

### Color distribution (from forecast.bio homepage)

Weighted random sampling matching the original site's `kConfetti` array:

| Color       | Hex       | Weight | Probability |
|-------------|-----------|--------|-------------|
| Warm grey   | `#ede2e4` | 25     | 57%         |
| Blue        | `#00A6DB` | 7      | 16%         |
| Yellow      | `#FFCE02` | 6      | 14%         |
| Pink        | `#FFB6C6` | 3      | 7%          |
| Red         | `#F95838` | 3      | 7%          |

Grey-dominant with colored accents. This keeps confetti subtle while the colored dots pop.

### Static confetti (SVG diagrams)

- Uniform radius (tunable via `CONFETTI_RADIUS` in `brand.py`, default: 5)
- Wrapped in `<g style="mix-blend-mode: multiply">`
- Uses solid brand colors only (red, green, blue, yellow, pink) — no grey in static diagrams

### Animated confetti (footer JS)

- 14 dots, 11px diameter
- Positioned in a **cone shape** emanating up-right from the Forecast wordmark
- Cone origin at logo baseline, slope drifts upward (-0.28 rise per px)
- Spread grows with distance (cone half-angle 0.22)
- Extra noise (±10px) on both axes for fuzzy edges
- Fade in left-to-right on scroll (IntersectionObserver, 50ms stagger)
- Random on every page load (`Math.random()`)

## Diagram Generation Pipeline

All diagrams generated programmatically via Python scripts in `scripts/`:

- `scripts/brand.py` — shared module: palette, primitives, confetti, typography CSS
- `scripts/generate-banner.py` — home page banner (1500×500)
- `scripts/generate-diagram-*.py` — per-guide diagrams
- `scripts/generate-card-icons.py` — 12 feature card icons (150×100 each)
- `scripts/generate-all.sh` — regenerates everything

### Diagram conventions

- Sentence case titles (not Title Case)
- Mono class only for actual code/commands
- Subheading (Times italic) for secondary labels
- Background: `#F9F4F5` (brand bg)
- Confetti in top corners, 4-6 dots per cluster
- Seeded RNG for reproducible output

## Card Icons

Small 2-4 shape compositions for feature cards, each evocative of the feature:
- Session Memory: linked ellipses (continuity)
- Local-First: stacked DB layers
- Multi-Agent: triangle of overlapping circles
- Hooks: tall rail with guided shape
- Swarm: honeycomb cluster
- Knowledge: fanned pages with search lens
- TUI: terminal with content lines
- Web Dashboard: browser with panels
- Containers: nested boxes
- Workflow: diagonal arrow flow
- Maintenance: interlocking gears
- Everywhere: dispersed shapes (breadth)

## Footer

Matches forecast.bio card pages:
1. Thin rule (`#ede3e3`, 220px)
2. Wordmark + animated confetti cone (same row, flex, aligned to baseline)
3. Copyright notice (`© 2026 Forecast Bio, Inc. · Built with Quarto`)

Footer is injected via `include-after-body` and JS-relocated into Quarto's `<main class="content">` for proper content-column alignment.

## Quarto Layout

- Two-column pattern: 35% / 5% spacer / 60% for chat-vs-command pages
- `code-overflow: scroll` (not wrap)
- `.column-page-right` on wider diagrams (>700px), wrapped in `:::` div fences
- Full-width banner on index via CSS viewport breakout
