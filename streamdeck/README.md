# AudioDuck for Stream Deck / OpenDeck

AudioDuck runs only as a plugin: no window, no tray. Stream Deck starts `audioduck.exe`, ducking runs while Stream Deck
runs, and when Stream Deck closes AudioDuck restores the volumes and exits. Stream Deck only starts a plugin once one
of its keys is on a profile.

One key, "Ducking":

- **Tap**: pause or resume ducking. The key shows the state: blue = active, orange = ducking right now, grey = paused.
- **Key settings** (property inspector, `ui/inspector.html`): rules, apps playing with live levels and volumes,
  threshold and timings. Config is still `%APPDATA%\AudioDuck\config.json`.

Install: `powershell -File streamdeck/install.ps1` (builds, copies the plugin into Stream Deck, restarts it).
For OpenDeck, copy `com.xenside.audioduck.sdPlugin` (with `bin/audioduck.exe`) into its plugins folder.
Icons: `cargo run --example icons`.
