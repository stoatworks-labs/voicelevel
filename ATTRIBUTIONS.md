# Attributions

VoiceLevel is built on other people's work. This file lists what that work is, who did
it, and what it is doing here.

It is generated — the master lists live in the `stoatworks-backend` repo and are
pushed out by `scripts/sync-attributions.py`. Edit it there, not here.

## Licensing: GPL-3.0, because of the VST3 bindings.

nih-plug itself is ISC, and its CLAP side is MIT/Apache. But VST3 is reached through `vst3-sys`, bindings to Steinberg's VST3 SDK that are licensed **GPLv3** — Steinberg's own terms for the SDK are proprietary-or-GPLv3, and nih-plug takes the GPL arm. A VST3 build of this plugin is therefore a GPLv3 work, and rather than licence the source under one thing and ship the binaries under another, this repo is **GPL-3.0-or-later** throughout.

What that changes:

- **Using the plugin** — nothing. Install it and run it on any show, commercial or not. The GPL constrains distribution, not use.
- **Reusing this repo's source** — GPL-3.0-or-later, as `LICENSE` says: derived work that is distributed must be GPL too, with its source available.
- **Redistributing the binaries** — the same, and the corresponding source is this repo at the tagged commit plus nih-plug at the commit pinned in `Cargo.lock`, both public.

This section states what these licences already require. It is not an extra restriction, and it is not legal advice.

## Third-party code this project uses

Libraries, SDKs and frameworks the project is built on or bundles.

### nih-plug

<https://github.com/robbert-vdh/nih-plug>  
Licence: ISC (the framework); GPLv3 (its VST3 bindings, vst3-sys)  
Copyright: Robbert van der Helm

A Cargo git dependency, pinned in Cargo.lock: nih_plug for the plugin wrappers, nih_plug_egui for the editor, nih_plug_xtask for the bundler.

The plugin framework: one Rust crate becomes a VST3 bundle, a CLAP bundle and a standalone app, with the parameter, state and editor plumbing each host expects.

### The Rust crate ecosystem

<https://crates.io>  
Licence: predominantly MIT or Apache-2.0  
Copyright: the individual crate authors

Cargo dependencies, resolved and pinned in Cargo.lock.

Async runtimes, protocol codecs, serialisation and GUI toolkits. The exact set and versions for any build are in that repo's Cargo.lock, which is the authoritative list.

The full transitive dependency set for any build is pinned in this repo's lockfile,
which is the authoritative list. What is named above is the layers a reader would
want to know about, not every package that has ever been resolved.

## Getting this wrong

If your work is here and the description is inaccurate, the licence is wrong, or you would rather not be listed — open an issue and it will be fixed.
