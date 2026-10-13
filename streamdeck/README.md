# AudioDuck for Stream Deck / OpenDeck

AudioDuck itself is the plugin: Stream Deck starts `audioduck.exe` (window hidden, tray icon as usual), ducking runs
while Stream Deck runs, and when Stream Deck closes AudioDuck restores the volumes and exits. Rules are edited in the
usual window.

One key, "Ducking":

- **Tap**: pause or resume ducking (same as the tray's "Ducking attivo").
- **Hold** (half a second): open the AudioDuck window.
- The key shows the state: blue = active, orange = ducking right now, grey = paused.

Stream Deck only starts a plugin once one of its keys is on a profile. If an AudioDuck started on its own is already
running, the plugin copy just becomes its remote (via the message-only window in `src/control.rs`) and leaves it alone.

Install: `powershell -File streamdeck/install.ps1` (builds, copies the plugin into Stream Deck, restarts it).
For OpenDeck, copy `com.xenside.audioduck.sdPlugin` (with `bin/audioduck.exe`) into its plugins folder.
Icons: `cargo run --example icons`.
