# AudioDuck for Stream Deck / OpenDeck

AudioDuck runs only as a plugin: no window, no tray. Stream Deck starts `audioduck.exe`, ducking runs while Stream Deck
runs, and when Stream Deck closes AudioDuck restores the volumes and exits. Stream Deck only starts a plugin once one
of its keys is on a profile.

One action, "Regola di ducking": **one key = one rule**.

- **Key settings** (`ui/rule.html`): set up the key's rule (which apps trigger it, which apps get quieter or "all the
  others", how much), or pick an existing rule, or delete it. Below: apps playing with live levels and volumes, and
  the threshold and timings, which are shared by all keys.
- **Tap**: turns the key's rule off and on. Title = the rule's trigger app(s); grey = off, blue = listening,
  orange = ducking right now. "Configura" = no rule set up yet.

Rules live in `%APPDATA%\AudioDuck\config.json` with a stable `id`; a key's settings are `{"rule": id}`. Removing a key
does not delete its rule (Stream Deck doesn't tell plugins a key was removed): delete it from the key's settings first,
or pick it again from another key.

Install: `powershell -File streamdeck/install.ps1` (builds, copies the plugin into Stream Deck, restarts it).
For OpenDeck, copy `com.xenside.audioduck.sdPlugin` (with `bin/audioduck.exe`) into its plugins folder.
Icons: `cargo run --example icons`.
