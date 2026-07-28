# Drift File Explorer — Design Guide

## Design direction

Drift is a modern Linux file explorer concept shaped by desktop-ricing aesthetics: compact, keyboard-friendly, visually quiet, and free of conventional window chrome. The interface should feel native beside a carefully themed terminal, status bar, and tiling window manager.

The design avoids logos, title bars, rounded cards, gradients, oversized controls, and decorative visual noise. Hierarchy comes from spacing, restrained color, typography, and thin separators.

## Core principles

1. **Content before chrome** — devote the interface to navigation and files rather than branding or window furniture.
2. **Dense but legible** — keep controls and rows compact without compromising scanability.
3. **Quiet by default** — reserve bright color for current location, selection, file types, and actionable feedback.
4. **Keyboard-ready** — expose familiar shortcuts and keep the layout predictable for fast navigation.
5. **Ricing-compatible** — use square geometry, dark neutral surfaces, monospace typography, and minimal borders.
6. **Functional minimalism** — every visible element should communicate state or support an action.

## Layout

The desktop layout has two primary regions:

- **Sidebar: 210 px** — places, mounts, tags, and storage status.
- **Main area: flexible** — navigation toolbar, directory contents, and status line.

The mockup is shown inside a border-only application surface with no title bar, logo, window buttons, or rounded decorations. In a real desktop app, the compositor or window manager should own any outer gaps and shadows.

### Responsive behavior

Below `780 px`, the sidebar collapses to a `58 px` icon rail. Text labels and storage details are hidden. Secondary file metadata is reduced so filenames retain priority.

## Spacing system

Use a compact 4 px-based spacing rhythm:

| Token | Value | Typical use |
|---|---:|---|
| XS | 4 px | Icon and micro-label adjustment |
| S | 8 px | Control internals and compact gaps |
| M | 12 px | Sidebar padding and row content |
| L | 16 px | Section separation |
| XL | 24 px | Main content inset |

Key dimensions:

- Toolbar height: `58 px`
- Sidebar navigation row: `34 px`
- File list row: `49 px`
- Search and view controls: `31 px`
- Status line: `35 px`
- File grid card: `114 px`

## Color palette

| Role | Hex | Usage |
|---|---|---|
| Canvas | `#090b0d` | Page or compositor backdrop |
| App background | `#0d0f12` | Main file surface |
| Sidebar | `#111419` | Navigation surface |
| Raised/hover surface | `#15191f` | Hovered controls and subtle elevation |
| Border | `#242a32` | Dividers and control outlines |
| Primary text | `#d8dee9` | Important readable content |
| Muted text | `#69727f` | Metadata and secondary labels |
| Accent | `#9ece6a` | Active location, selection, feedback |
| Accent surface | `#1c291c` | Selected and active backgrounds |
| Folder blue | `#7aa2f7` | Directory icons |
| Image violet | `#bb9af7` | Image/design file icons |
| Document cyan | `#7dcfff` | Document icons |
| Video red | `#f7768e` | Video icons and important tag |
| Archive amber | `#e0af68` | Archive icons and active tag |

### Color rules

- Keep large surfaces near-black rather than pure black.
- Use the green accent sparingly; it should indicate state, not decorate.
- Metadata should remain subdued until its row is selected.
- File-type colors belong to icons only, preserving calm filenames and rows.
- Maintain sufficient contrast for primary text and interactive focus states.

## Typography

Use a monospace stack to fit Linux-rice environments:

```css
font-family: "JetBrains Mono", "Iosevka", "Cascadia Code", monospace;
```

- Base size: `13 px`
- Base line height: `1.4`
- Directory heading: `16 px`, semibold
- Navigation labels: `13 px`
- Section and table labels: `9 px`, uppercase, expanded tracking
- Metadata and status text: `9–10 px`

Typography should remain lowercase where it supports the terminal-inspired visual language. Filenames retain their actual case in a production implementation.

## Components

### Sidebar

The sidebar begins directly with the `places` section. There is no product mark or decorative header.

- Active rows use a 2 px accent rule on the left.
- Active text and icon use the accent color.
- Hover uses a subtle raised surface without animation-heavy effects.
- Counts and capacities align to the far edge in muted text.
- Storage usage sits at the bottom and uses a 2 px progress bar.

### Navigation toolbar

The toolbar contains, in order:

1. Back and forward controls
2. Breadcrumb path
3. Flexible spacer
4. File filter/search
5. List/grid view switch
6. New-directory action

Controls have square corners and no permanent fill unless selected. The breadcrumb follows shell-path conventions (`~/home`).

### Search

The search field is a quiet bordered control. On focus, it grows from `200 px` to `245 px` and slightly strengthens its border. Filtering updates the visible directory entries immediately.

### File list

- The filename column receives the most width (`53%`).
- Metadata columns are modified time, kind, and size.
- Headers are small, uppercase, and letter-spaced.
- Rows use thin separators rather than cards.
- Hover changes only the row surface.
- Selection uses the accent surface and accent filename color.

### Grid view

Grid cards preserve square geometry and use only a thin outline on hover. Icons become larger, while labels remain single-line and ellipsized. Selection matches list-view semantics.

### Status line

The bottom line communicates entry count or selection state. Keyboard hints appear on the right with minimal keycap styling.

### Feedback

Transient feedback uses an accent-tinted notification near the bottom of the app. It should appear quickly, remain briefly, and disappear without blocking interaction.

## Interaction states

Every interactive component should support:

- **Default** — neutral and visually quiet
- **Hover** — subtle surface or text-color shift
- **Focus** — visible outline suitable for keyboard navigation
- **Active/selected** — accent color plus accent-tinted surface
- **Disabled** — reduced contrast with no hover response

Production focus styling recommendation:

```css
:focus-visible {
  outline: 1px solid #9ece6a;
  outline-offset: 2px;
}
```

## Iconography

Use simple 16–18 px line icons with approximately 1.5 px stroke weight. Avoid filled, illustrative, or multicolor icon sets. Sidebar glyphs should have consistent optical size even when symbols differ.

Recommended production choices include Lucide, Tabler Icons, or a purpose-built symbolic Linux icon set. Do not mix icon families in the final implementation.

## Motion

Motion should be brief and functional:

- Hover/focus transitions: `120–180 ms`
- Search expansion: approximately `180 ms`
- Toast entrance/exit: approximately `200 ms`
- No bouncing, spring effects, or decorative ambient animation

Respect `prefers-reduced-motion` in production.

## Accessibility and implementation notes

- Preserve semantic buttons, table markup, labels, and accessible names.
- Ensure all navigation and file actions are keyboard reachable.
- Do not communicate file type or selection by color alone.
- Provide high-contrast and light theme variants through the same design tokens.
- Use the system locale for dates, times, and file sizes.
- Support long filenames with ellipsis plus a full-name tooltip or details view.
- In the actual Linux app, prefer compositor-managed shadows and borders instead of embedding decorative window chrome.

## Source mockup

The companion `drift-file-explorer.html` file is a standalone interactive mockup with inline CSS and JavaScript. It requires no build step or external assets and can be opened directly in a browser.
