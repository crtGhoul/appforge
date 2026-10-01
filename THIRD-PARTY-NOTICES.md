# Third-Party Notices

AppForge itself is MIT-licensed. This file attributes third-party components
distributed with it (as direct dependencies).

## adblock (Brave adblock-rust)

- Crate: `adblock` (v0.13.x) — https://crates.io/crates/adblock
- Source: https://github.com/brave/adblock-rust
- License: **Mozilla Public License 2.0 (MPL-2.0)**
- Used for: network-level ad/tracker request blocking and cosmetic-filter
  selector generation, matched in AppForge's native backend against the
  EasyList and EasyPrivacy filter lists.

The MPL-2.0 is a file-level copyleft license: it applies to the `adblock`
crate's own files only. AppForge uses it as an unmodified dependency, so
AppForge's own code remains MIT-licensed. The full MPL-2.0 text is available
at https://www.mozilla.org/en-US/MPL/2.0/ and in the crate's repository.
