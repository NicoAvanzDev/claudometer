# Repository Guidelines

## Project Overview

This repository contains `CCTaskBarUsage`, a native Windows C++ taskbar overlay that displays Claude Code usage information. The Visual Studio solution is `CCTaskBarUsage.slnx`, and the main implementation lives in `CCTaskBarUsage/CCTaskBarUsage.cpp`.

## Development Notes

- Keep changes scoped and consistent with the existing Win32 style.
- Prefer small, direct functions over broad refactors.
- Use Unicode-aware Win32 APIs where the project already uses wide strings.
- Do not commit generated Visual Studio output unless explicitly requested.
- Build artifacts are under `x64/` and `CCTaskBarUsage/x64/`.

## Build

Use Visual Studio or MSBuild from a Developer PowerShell:

```powershell
msbuild CCTaskBarUsage.slnx /p:Configuration=Debug /p:Platform=x64
msbuild CCTaskBarUsage.slnx /p:Configuration=Release /p:Platform=x64
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
