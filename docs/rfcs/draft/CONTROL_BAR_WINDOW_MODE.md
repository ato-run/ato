# Control Bar Window Mode

## Status

Draft

## Context

Ato Desktop Focus View treats the Control Bar as a process-global command
surface, not as chrome embedded inside each capsule window. The bar follows the
most-recently-focused Ato content window through `OpenContentWindows` and
changes its command target accordingly.

An independent window reduces per-app clutter, but it can also become lost or
intrusive. Users need explicit display modes plus recovery paths from Settings
and the macOS menu bar.

## Decision

Persist `desktop.control_bar.mode` as the canonical display mode:

- `floating`: the full Control Bar pill is visible.
- `auto-hide`: the compact pill is visible by default and expands for direct
  interaction.
- `compact-pill`: only status, active target, Stop, and URL focus affordance are
  shown.
- `hidden`: the Control Bar window is closed, but can be restored through menu
  actions or keyboard shortcuts.

Legacy `visible_on_startup` and `auto_hide` remain readable for compatibility.
When `mode` is absent, old configs map to `hidden` if `visible_on_startup=false`,
`auto-hide` if `auto_hide=true`, and `floating` otherwise.

## Command Surface

The Control Bar exposes app-level actions:

- `ShowControlBar`
- `HideControlBar`
- `ToggleControlBar`
- `FocusControlBarInput`
- `SetControlBarMode`

Keyboard shortcuts are app-level, not OS-global in this draft:

- `Cmd/Ctrl+Shift+B`: toggle visibility.
- `Cmd/Ctrl+L`: show the bar and focus URL/capsule input.

The macOS menu bar provides recovery and safety actions: Show/Hide Control Bar,
Control Bar Mode, Open Store, Open Settings, Stop Active Capsule, and Stop All
Capsules. Windows/Linux tray support should use the same actions through a
platform adapter.

## Safety

The active operation target is the MRU entry in `OpenContentWindows`. The bar
must display that target in both expanded and compact states. Stop actions close
the active capsule AppWindow in Focus View; `AppCapsuleShell::Drop` is
responsible for aborting launch or stopping the running session.
