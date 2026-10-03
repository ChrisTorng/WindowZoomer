# WindowZoomer

A small, fast Windows utility that magnifies the window you were using before WindowZoomer came to the foreground, then automatically follows whichever non-WindowZoomer window becomes foreground later.

## Behavior

- On launch, remembers the foreground window that was active immediately before WindowZoomer creates its own window.
- Captures that target with Windows Graphics Capture (WGC / D3D11).
- If another normal application window becomes foreground, WindowZoomer switches capture to it.
- Returning to WindowZoomer keeps the last captured target.
- The viewer is a normal resizable window; it does not force fullscreen.
- Default view is **Fit**: preserve aspect ratio and enlarge the target as much as possible.
- Scaling is nearest-neighbor / pixelated; no smoothing is applied.

## Controls

| Action | Mouse | Keyboard |
| --- | --- | --- |
| Zoom in | Wheel up / `+` button | `+`, `=`, Numpad `+` |
| Zoom out | Wheel down / `-` button | `-`, Numpad `-` |
| Fit | **Fit** button | `0` |
| Pan | Left-drag | Arrow keys |
| Pan faster | — | `Shift` + Arrow keys |
| Screenshot current viewport | **Screenshot** button | `Ctrl+S` |

Mouse-wheel zoom is anchored at the mouse pointer.

## Screenshots

**Screenshot** saves exactly the current image viewport, including the current zoom and pan, to:

`Pictures\Screenshots\Screenshot YYYY-MM-DD HHMMSS.png`

If a file with the same timestamp already exists, a Windows-style numeric suffix is added.

## Build

Prerequisite: Rust stable with the MSVC Windows target.

```powershell
cargo build --release
```

Output:

`target\release\windowzoomer.exe`

GitHub Actions also builds a release executable and uploads it as the **WindowZoomer-windows-x64** workflow artifact.

## Requirements

Windows 10 version 1903 or newer is recommended because WindowZoomer uses Windows Graphics Capture.

## License

MIT
