# <img src="claudometer-icon.png" alt="Claudometer icon" width="32" height="32"> Claudometer

[![Build](https://img.shields.io/github/actions/workflow/status/NicoAvanzDev/claudometer/ci.yml?branch=master&job=build&label=build)](https://github.com/NicoAvanzDev/claudometer/actions/workflows/ci.yml)
[![Tests](https://img.shields.io/github/actions/workflow/status/NicoAvanzDev/claudometer/ci.yml?branch=master&job=tests&label=tests)](https://github.com/NicoAvanzDev/claudometer/actions/workflows/ci.yml)

A native Windows taskbar overlay widget written in Rust that displays Claude Code usage statistics—session (5h) and weekly (7d) token consumption percentages.

![Claudometer taskbar widget](claudometer-widget.png)

## Features

- **Native taskbar overlay**: Embeds directly in the Windows taskbar instead of floating above it
- **Claude Code usage meter**: Shows 5-hour session and 7-day weekly utilization from Anthropic rate-limit headers
- **Reset hints**: Displays compact reset labels for the current session and weekly window
- **Background refresh**: Polls usage every 5 minutes and refreshes after workstation unlock
- **Multi-monitor support**: Creates a widget for each detected Windows taskbar
- **Tray controls**: Provides quick reload, log folder access, and quit actions from the notification area
- **Update notifications**: Checks the latest GitHub release hourly and opens the release page when clicked

## Building

Requires Windows 10+ with the MSVC Rust toolchain. From Developer PowerShell or another shell with the MSVC Rust toolchain available:

```powershell
cargo build
cargo build --release
```

The release binary is `target/release/claudometer.exe`.

## Installer

Download the latest Windows installer from the [GitHub Releases page](https://github.com/NicoAvanzDev/claudometer/releases/latest).

The Windows installer is built with [Inno Setup 6](https://jrsoftware.org/isinfo.php).

```powershell
cargo build --release
iscc /DMyAppVersion=1.5.0 installer\claudometer.iss
```

The installer is written to `dist/Claudometer-Setup-1.5.0.exe`. It installs per-user under `%LOCALAPPDATA%\Programs\Claudometer`, creates a Start Menu shortcut, and starts Claudometer automatically at sign-in through `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.

GitHub Actions builds and uploads the installer automatically when a tagged GitHub release is published.

## Architecture

The app is organized into focused modules:

- **`app.rs`**: Application lifecycle, window class registration, message loop
- **`credentials.rs`**: Claude Code credential discovery and OAuth token refresh
- **`diagnostics.rs`**: Local logging, panic hooks, and crash diagnostics
- **`drawing.rs`**: Direct2D rendering: icon, usage bars, reset labels, and status text
- **`taskbar.rs`**: Taskbar window discovery (`Shell_TrayWnd`, secondary taskbars)
- **`tray.rs`**: Notification area icon, context menu, update balloon handling
- **`updates.rs`**: GitHub release checks
- **`usage.rs`**: API polling and usage snapshot management
- **`main.rs`**: Entry point, error handling
- **`widget.rs`**: Widget window creation, positioning, timers, and message handling
- **`winstr.rs`**: UTF-16 string helpers for Win32 APIs

## Widget Z-Order Fix (Windows 11 Compatibility)

The widget is embedded as a true child of `Shell_TrayWnd` (the taskbar window), not a free-floating topmost popup. This ensures the widget's z-order is tied to the taskbar: when the shell raises the taskbar on user interaction, the widget rises with it and is never buried underneath.

**Why this matters**: Prior attempts to keep the widget above the taskbar using `WS_EX_TOPMOST` failed because both the widget and taskbar live in the "topmost" band, where Explorer controls relative z-order. Embedding as a child eliminates this z-order battle structurally.

**Windows 11 rendering quirk**: Plain child windows render into the taskbar's legacy GDI surface, which DWM composites *below* the XAML content covering the modern Windows 11 taskbar. The widget is marked with `WS_EX_LAYERED` (via post-creation `SetWindowLongPtrW`) to get its own DWM surface stacked above the taskbar's visible content. A Windows 8+ compatibility manifest is required; without it, the OS silently strips the layered style.

## Configuration

No Claudometer configuration file is needed. The widget:

- Fetches usage stats every 5 minutes
- Checks for new GitHub releases every hour
- Polls for new data on session unlock
- Positions itself in the taskbar's left corner (horizontal) or top corner (vertical, for pinned taskbars)
- Uses the Claude Code branding icon and color scheme (orange for session, blue for weekly)

## API Integration

Claudometer reads the local Claude Code OAuth credential and uses it to make a minimal request to Anthropic's Messages API. It derives usage from the `anthropic-ratelimit-unified-5h-*` and `anthropic-ratelimit-unified-7d-*` response headers.

Credential lookup order:

1. `CLAUDE_CREDENTIALS_PATH`, when set
2. `CLAUDE_CONFIG_DIR\.credentials.json`, when set
3. `%USERPROFILE%\.claude\.credentials.json`
4. `%LOCALAPPDATA%\Claude\.credentials.json`
5. `%APPDATA%\Claude\.credentials.json`

If the credential contains a refresh token and expiry timestamp, Claudometer refreshes the access token shortly before expiry. If no usable credential is found, the widget shows a `no token` status. Sign in with Claude Code first, then reload Claudometer from the tray menu.

## Tray Menu

The notification area icon exposes:

- **Reload widget**: Recreates widgets and starts a fresh usage fetch
- **Open logs folder**: Opens the local `logs` directory next to the executable
- **Quit**: Removes the tray icon and destroys the widgets

## Verification

After building, run the release binary and observe:

1. The widget appears on all detected taskbars
2. It displays current usage percentages (or a loading/error state)
3. Clicking taskbar items does not cause the widget to disappear
4. The tray menu can reload the widget and open the log folder
5. Usage data refreshes after 5 minutes or on session unlock

For debugging:

- Monitor the release binary process in Task Manager (`claudometer.exe`)
- Check `logs\claudometer.log` next to the executable
- Verify taskbar window handles with Win32 inspection tools

## Notes

- The app exits when the taskbar is destroyed (e.g., Explorer crash/restart). To recover, simply relaunch it.
- The widget's message queue is attached to Explorer's, so a hang in the widget's window procedure could hang the taskbar. The procedure is minimal (timers and Direct2D painting) so this is not a practical concern.
- Multi-monitor support is automatic; the app discovers all taskbars on startup and creates a widget for each.
- A single-instance mutex prevents duplicate Claudometer processes from starting.
