# SolidCraft GUI (staging acceptance)

Both Derivations launch the SolidCraft 0.43.0 CAD window on macOS/aarch64.
D1 downloads the signed upstream DMG; D2 builds the pinned `solidcraft` GUI
package with managed Rust 1.95.0. The upstream is Tangency Labs:
https://github.com/tangency-labs/solidcraft/tree/v0.43.0

This sample is distinct from the headless CLI sample in `../solidcraft`.
The GUI Contract observes an actual window owned by the Run. CAD editing,
saving and reopening require the separate interactive acceptance record.

Install and open with `ato app install <file.capsule|capsules/owner/name> --derivation <D>`.
Restart the returned Instance with `ato app start <instance>`; D2 additionally
requires `--allow-native-source-build --allow-unsigned-native` on each start.
The Instance's filesystem `home` state is the application HOME and working
directory. Save designs there so they survive stop/start. The application runs
under mandatory Seatbelt containment, with no runtime internet access.
`ato app stop <instance>` stops the application and snapshots its saved data.
`ato run` is a disposable session and is not the installation command.

The capsule carries no upstream executable or brand images. D1 references
upstream bytes by digest; D2 locally compiles unmodified pinned source.
