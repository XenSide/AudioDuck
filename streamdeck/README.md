# AudioDuck for Stream Deck / OpenDeck

AudioDuck runs only as a plugin: no window, no tray. Stream Deck starts `audioduck.exe`, ducking runs while Stream Deck
runs, and when Stream Deck closes AudioDuck restores the volumes and exits. Stream Deck only starts a plugin once one
of its keys is on a profile.

One action, "Ducking": **each key is set up on its own**, and two identical keys do the same thing.

- **Key settings** (`ui/rule.html`): which apps trigger the key, which apps get quieter (or "all the others") and how
  much, plus its style. Every change is saved at once. Below: apps playing with live levels and volumes, and
  the threshold and timings, which are shared by all keys.
- **Key image** (Foreground Boost's design, `src/keyimage.rs`): the trigger app's icon and the ducked volume
  (`⌄ 20%`). Dimmed with "Off" when the key is off, a filled pill while it is ducking, "Set up" until it has
  something to do. Style per key: Default (stock Stream Deck look) or Duotone (blue, orange, purple, red or custom colours,
  app icons recoloured to match).
- **Tap**: turns the key off and on.

A key's own Stream Deck settings are what it does (`sources`, `targets`, `all_others`, `level`, `enabled`, style), so
deleting the key deletes its setup. Keys on other pages and profiles are read from Stream Deck's saved profiles, re-read
when Windows reports Stream Deck saving one (Elgato Stream Deck only; elsewhere only on-screen keys duck).
`%APPDATA%\AudioDuck\config.json` holds only the threshold and timings shared by all keys.

Install: `powershell -File streamdeck/install.ps1` (builds, copies the plugin into Stream Deck, restarts it).
For OpenDeck, copy `com.xenside.audioduck.sdPlugin` (with `bin/audioduck.exe`) into its plugins folder.
Icons: `cargo run --example icons`.
