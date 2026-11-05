radix-LICENSE.txt: radix-ui/primitives LICENSE at commit
fcef0668a5c827e5a4baac405474d75680f9a4eb (the published
@radix-ui/react-context 1.1.2 gitHead). The other Radix npm packages in this
build that omit license files report the same gitHead in npm metadata.

react-remove-scroll-bar-LICENSE.txt: theKashey/react-remove-scroll-bar LICENSE
at commit 8ca9ba5ea52de03308fe8ced94f7b159a44d28ff. The npm package 2.3.8
declares MIT and omits the file; its published gitHead was not found in that
repository. Verify the historical notice before production distribution.

tauri-MIT.txt: copied from @tauri-apps/api 2.10.1 LICENSE_MIT. Tauri plugin
packages declare MIT OR Apache-2.0 but contain SPDX metadata rather than the
full license text.
