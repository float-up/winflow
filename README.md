# winflow

[简体中文](README.zh-CN.md) | **English**

> A lightweight macOS window switcher, written in Rust.

winflow fills the gaps in the system's built-in `⌘Tab`: live window thumbnails, grouping by desktop (Space), MRU ordering, and multiple ways to move around — `hjkl`, arrow keys, or the mouse. It's light on resources and instant to summon, even on first use.

## Design Philosophy

Window switchers keep getting richer in features, yet the one thing that actually matters — switching from one window to another — is often the worst part: the moment your desktop gets busy, they lag and stutter. That's getting the priorities backwards. winflow does one thing, and does it well:

- **Two fixed hotkeys, overriding the system.** There are only two: `⌘Tab` (all active windows on the current desktop) and ````⌘\` ```` (all windows of the frontmost app on the current desktop). The system switcher is intercepted at the HID layer, so there are no hotkeys to configure — it just works out of the box.
- **Active windows only.** The switcher only ever shows the active windows on the current display's current desktop — nothing hidden with `⌘M`, nothing minimized, and nothing merely closed with `⌘W` (the app itself still running).
- **Thumbnails are for locating, not for live preview.** The overlay shows a thumbnail of every active window on the desktop so you can see exactly where you're going. That's why they don't need to be real-time: they're captured asynchronously in the background — a full pass warms the cache at startup, and afterwards only changed windows are refreshed on a fixed schedule, so we never spam the screen-capture API.
- **⌘ is everything.** Tap `⌘Tab` / ````⌘\` ```` and release ⌘ quickly → instant switch back to the previous window, no UI at all. Hold ⌘ → the overlay appears and the selection follows you; release ⌘ → the highlighted window is activated automatically. No mouse, no Enter, no explicit confirmation.
- **Keep your right hand on the keyboard.** The overlay is designed to minimize mouse travel: `hjkl` Vim-style navigation is recommended, with arrow keys, `Tab` / `Shift+Tab`, the scroll wheel, and hover-to-select (no click needed) as alternatives.
- **A menu bar with just two items.** "Configure…" and "Quit winflow". The menu bar icon mostly exists just to tell you the app is still running.
- **No unnecessary options.** A focused tool should be effortless — winflow ships with sane defaults and works out of the box.
- **Performance first.** The execution path is heavily optimized so that even a desktop full of windows switches without stutter — on first summon, on window changes, and everywhere in between.

This project is built entirely with Pi + DeepSeek V4, with no Pi plugins or skills installed.

## Features

- **Background thumbnail capture, pre-warmed at launch.** A background thread starts capturing visible windows from the moment the app starts and caches them in shared memory, so the overlay has thumbnails ready on the very first summon — no lag. The refresh interval defaults to **45s** and can be adjusted in the "Configure…" panel (1–3600s); the setting is **persisted across restarts**.
- **Quick switching.** Tap `⌘Tab` / ````⌘\` ```` (release ⌘ within the judgment delay) to jump straight back to the previous window without showing the overlay; hold longer to bring it up. The delay defaults to **0.08s** and is adjustable in the panel.
- **The overlay follows ⌘.** Hold ⌘ and the thumbnails stay on screen (even with nothing selected); **release ⌘ and the selected window is activated automatically** — no Enter or click required.
- **MRU ordering.** The list is ordered by recency — switch from A to B, summon again and A is pre-selected; press once more to get back to B. Fast round-trips between two (or more) windows.
- **Equal-height, variable-width thumbnails.** Height is fixed and width follows each window's aspect ratio, so a row can fit several windows.
- **Multiple navigation styles.** Arrow keys / `hjkl` / `Tab` / `Shift+Tab` / scroll wheel, with **edge wrapping**.
- **Hover to select.** The mouse behaves exactly like the keyboard — hover to select with a highlight box that follows instantly, click to switch.
- **Junk-window filtering.** Windows that shouldn't be switch targets are filtered out automatically (e.g., Feishu's watermark layer: untitled, fully contained by a larger window of the same app, transparent, or too small).

## Building and Running

```bash
cargo build --release
./target/release/winflow          # run in the foreground (Ctrl+C to quit)
```

## Packaging as a .app

```bash
./package.sh              # build & package to dist/winflow.app (icon, Info.plist, ad-hoc signing)
./package.sh --install    # package and install to /Applications
./package.sh --open       # package and launch
```

After the first run, grant winflow **Accessibility** and **Screen Recording** permissions in System Settings → Privacy & Security.

The icon source lives in `assets/icon.png` — replace it and re-package.

Development / debugging:

```bash
./target/release/winflow --show               # pop the switcher once after 2s
./target/release/winflow --panel              # open the settings panel after 2s
./target/release/winflow --force-perm-dialog  # force the permission dialog (for UI testing)
WINFLOW_RENDER_OUT=/tmp/out.png ./target/release/winflow --show  # dump the composed overlay to a PNG
```

## Permissions (Required)

**winflow checks for the following two permissions at every launch**, and proactively shows a dialog with buttons that jump straight to the relevant System Settings pane:

1. **Accessibility** — to intercept `⌘Tab` / ````⌘\` ```` at the HID layer (overriding the system switcher) and to raise/focus windows (AX).
2. **Screen Recording** — to display window thumbnails and titles.

> After granting permission, restart winflow: when run from the command line, the permission is granted to your terminal; when run as a .app, it's granted to winflow itself.

## Usage

| Key | Action |
| --- | --- |
| `⌘Tab` tap | Switch back to the previous window (no overlay) |
| `⌘Tab` hold | Open mode 1 (all active windows on the current desktop, overriding the system); when already open, advance to the next |
| ````⌘\` ```` tap / hold | Same as above, for the frontmost app's windows |
| `⌘⇧Tab` | Open mode 1 (backwards) |
| `Tab` / `Shift+Tab` | Next / previous |
| `←↑↓→` / `hjkl` | Move through the grid (edge wrapping) |
| `Return` | Switch to the selected window |
| `Esc` | Cancel |
| Mouse | Hover to select, click to switch; click empty space to cancel |
| Scroll wheel | Previous / next |
| Release `⌘` | Switch to the highlighted window and close the overlay |

Menu bar icon → "Configure…" panel:

- **Thumbnail refresh interval** (seconds, 1–3600, default **45**)
- **Hotkey judgment delay** (seconds, 0.05–2.0, default **0.08**) — release ⌘ faster than this to switch straight to the previous window; slower to show the overlay

Clicking "OK" writes the settings to `~/Library/Application Support/winflow/settings.conf` (plain text, persisted across restarts; delete the file to reset to defaults).

"Quit winflow" exits the app and releases the system hotkeys.

## How It Works (in a Nutshell)

- A global `CGEventTap` (HID layer) intercepts `⌘Tab` / ````⌘\` ```` and the navigation keys, mutating only the shared `Core` state and dispatching commands to the main thread.
- The main thread composes the whole grid into a single NSImage (bottom-left coordinates) inside a borderless, transparent NSWindow (level 101, joinable on all Spaces).
- Background threads grab window images on a schedule (`CGWindowListCreateImage` + immediate downscale), cached by window id; the main thread rebuilds the NSImage on generation invalidation, so a cached thumbnail makes the first summon instant.
- Activation: AX raise + focus the target window, falling back to app activation (`activateWithOptions(.ActivateAllWindows)`).

Detailed design constraints and engineering conventions: [AGENTS.md](AGENTS.md).
