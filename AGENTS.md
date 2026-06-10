# Repository Guidelines

## Project Overview

This repository contains `CCTaskBarUsage`, a native Windows Rust taskbar overlay that displays Claude Code usage information. The Cargo package is defined in `Cargo.toml`, with the Windows app split across focused modules under `src/`.

## Development Notes

- Keep changes scoped and consistent with the existing Win32/Rust style.
- Prefer small, direct functions over broad refactors.
- Use Unicode-aware Win32 APIs where the project uses wide strings.
- Use a normal Rust HTTP client for Anthropic calls; keep Win32 usage focused on the taskbar overlay and desktop integration.
- Do not commit generated Cargo or Windows build output unless explicitly requested.
- Build artifacts are under `target/`.

## Build

Use Cargo from a Developer PowerShell or a shell with the MSVC Rust toolchain available:

```powershell
cargo build
cargo build --release
```

## Verification

After code changes, build the affected configuration. For behavior changes, run the generated executable and verify the overlay on the Windows taskbar. Pay particular attention to:

- startup behavior
- taskbar positioning
- session lock/unlock behavior
- usage refresh timing
- shutdown cleanup

## Git Hygiene

- Review `git status --short` before editing and before finalizing work.
- Do not revert unrelated local changes.
- Keep commits focused on one logical change.
