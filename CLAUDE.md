# voicelevel (VoiceLevel)

Attenuation-only automatic mic leveler: a virtual fader that only ever turns a hot mic down.
Rust / nih-plug; VST3 + CLAP + standalone. Public repo, GPL-3.0. See AGENTS.md for the why.

## Commands
- Test the leveler: `cargo test`
- Build the bundles: `cargo xtask bundle voicelevel --release` → `target/bundled/VoiceLevel.{vst3,clap,app}`
- Universal macOS: `cargo xtask bundle-universal voicelevel --release` (needs both Apple rustup targets)
- Standalone on a device: `target/bundled/VoiceLevel.app/Contents/MacOS/VoiceLevel --backend core-audio -l 1 --input-device "<in>" --output-device "<out>"`
- Host validation: `/Applications/pluginval.app/Contents/MacOS/pluginval --strictness-level 10 --validate target/bundled/VoiceLevel.vst3`

## Rules
- Never add a gain path above unity, never add look-ahead. Both are the product.
- The audio thread is allocation-free (`assert_process_allocs` is on) and cannot set parameters — the fader is persisted state, Learn is a Mode value.
- `src/about_*` and `scripts/release-lib.sh` are vendored/generated from stoatworks-backend; edit the masters and re-sync.
- `docs/USER-GUIDE.md` is the only copy of the guide anyone edits; the PDF and site page are built from it.
- "Commit" = commit **and** push.
