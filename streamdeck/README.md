# AudioDuck for Stream Deck / OpenDeck

One key, "Ducking":

- **Tap**: pause or resume ducking (same as the tray's "Ducking attivo").
- **Hold** (half a second): open the AudioDuck window.
- The key shows the state: blue = active, orange = ducking right now, grey = paused, dark = AudioDuck not running.
  When it's dark a tap starts AudioDuck in the tray.

The plugin is `audioduck-streamdeck.exe` (`src/streamdeck.rs`). It talks to the running AudioDuck through a
hidden message-only window (`src/control.rs`), so AudioDuck keeps working on its own when Stream Deck is closed.

Install: `powershell -File streamdeck/install.ps1` (builds, copies the plugin into Stream Deck, restarts it).
For OpenDeck, copy `com.xenside.audioduck.sdPlugin` (with `bin/audioduck-streamdeck.exe`) into its plugins folder.
Icons: `cargo run --example icons`.
