# AudioDuck for Stream Deck / OpenDeck

AudioDuck runs only as a plugin: no window, no tray. Stream Deck starts `audioduck.exe`, ducking runs while Stream Deck
runs, and when Stream Deck closes AudioDuck restores the volumes and exits. Stream Deck only starts a plugin once one
of its keys is on a profile.

One action, "Ducking Rule": **one key = one rule**.

- **Key settings** (`ui/rule.html`): set up the key's rule and its style (which apps trigger it, which apps get quieter or "all the
  others", how much). Below: apps playing with live levels and volumes, and
  the threshold and timings, which are shared by all keys.
- **Key image** (Foreground Boost's design, `src/keyimage.rs`): the trigger app's icon and the ducked volume
  (`⌄ 20%`). Dimmed with "Off" when the rule is off, a filled pill while it is ducking, "Set up" when the key has no
  rule yet. Style per key: Default (stock Stream Deck look) or Duotone (blue, orange, purple, red or custom colours,
  app icons recoloured to match).
- **Tap**: turns the key's rule off and on.

Rules live in `%APPDATA%\AudioDuck\config.json` with a stable `id`; a key's settings are `{"rule": id}`. Removing a key
removes its rule: Stream Deck doesn't tell plugins a key was removed, so a thread waits on a Windows change notification
for Stream Deck's profile folders and deletes rules no key in any saved profile uses (Elgato Stream Deck only).

Install: `powershell -File streamdeck/install.ps1` (builds, copies the plugin into Stream Deck, restarts it).
For OpenDeck, copy `com.xenside.audioduck.sdPlugin` (with `bin/audioduck.exe`) into its plugins folder.
Icons: `cargo run --example icons`.
