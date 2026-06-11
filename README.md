# Claudometer

A native Windows taskbar overlay widget written in Rust that displays Claude Code usage statistics—session (5h) and weekly (7d) token consumption percentages.

## Features

- **Minimal overlay**: Embeds directly in the Windows taskbar, always visible and never obstructive
- **Real-time updates**: Refreshes usage stats on a configurable interval
- **Lock/unlock awareness**: Detects session lock/unlock events and fetches fresh data when the workstation unlocks
- **Multi-monitor support**: Works across all taskbars on multi-display setups
- **Zero external dependencies**: Uses only Win32 APIs and Rust's std library

## Building

Requires Windows 10+ with the MSVC Rust toolchain:

```powershell
cargo build --release
```

The release binary is `target/release/claudometer.exe`.

## Installer

The Windows installer is built with [Inno Setup 6](https://jrsoftware.org/isinfo.php).

```powershell
cargo build --release
iscc /DMyAppVersion=1.1.1 installer\claudometer.iss
```

The installer is written to `dist/Claudometer-Setup-1.1.1.exe`. It installs per-user under `%LOCALAPPDATA%\Programs\Claudometer`, creates a Start Menu shortcut, and starts Claudometer automatically at sign-in through `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.

GitHub Actions builds and uploads the installer automatically when a tagged GitHub release is published.

## Architecture

The app is organized into focused modules:

- **`app.rs`**: Application lifecycle, window class registration, message loop
- **`taskbar.rs`**: Taskbar window discovery (`Shell_TrayWnd`, secondary taskbars)
- **`widget.rs`**: Widget window creation, positioning, and message handling
- **`drawing.rs`**: Direct2D rendering: icon, usage bars, text labels
- **`usage.rs`**: API polling and usage snapshot management
- **`credentials.rs`**: Secure credential storage and retrieval
- **`main.rs`**: Entry point, error handling

## Widget Z-Order Fix (Windows 11 Compatibility)

The widget is embedded as a true child of `Shell_TrayWnd` (the taskbar window), not a free-floating topmost popup. This ensures the widget's z-order is tied to the taskbar: when the shell raises the taskbar on user interaction, the widget rises with it and is never buried underneath.

**Why this matters**: Prior attempts to keep the widget above the taskbar using `WS_EX_TOPMOST` failed because both the widget and taskbar live in the "topmost" band, where Explorer controls relative z-order. Embedding as a child eliminates this z-order battle structurally.

**Windows 11 rendering quirk**: Plain child windows render into the taskbar's legacy GDI surface, which DWM composites *below* the XAML content covering the modern Windows 11 taskbar. The widget is marked with `WS_EX_LAYERED` (via post-creation `SetWindowLongPtrW`) to get its own DWM surface stacked above the taskbar's visible content. A Windows 8+ compatibility manifest is required; without it, the OS silently strips the layered style.

## Configuration

No configuration file is needed. The widget:
- Fetches usage stats every 5 minutes
- Polls for new data on session unlock
- Positions itself in the taskbar's left corner (horizontal) or top corner (vertical, for pinned taskbars)
- Uses the Claude Code branding icon and color scheme (orange for session, blue for weekly)

## API Integration

The widget fetches usage data from the Claude Code backend using a stored access token. Tokens are stored securely using Windows credential storage APIs (`CredentialManager`). On first launch or after token expiry, the app will prompt for re-authentication.

## Verification

After building, run the release binary and observe:

1. The widget appears on all detected taskbars
2. It displays current usage percentages (or a loading/error state)
3. Clicking taskbar items does not cause the widget to disappear
4. Usage data refreshes after 5 minutes or on session unlock

For debugging:

- Monitor the release binary process in Task Manager (`claudometer.exe`)
- Check Windows Event Viewer for any critical errors
- Verify taskbar window handles with Win32 inspection tools

## Notes

- The app exits when the taskbar is destroyed (e.g., Explorer crash/restart). To recover, simply relaunch it.
- The widget's message queue is attached to Explorer's, so a hang in the widget's window procedure could hang the taskbar. The procedure is minimal (timers and Direct2D painting) so this is not a practical concern.
- Multi-monitor support is automatic; the app discovers all taskbars on startup and creates a widget for each.
