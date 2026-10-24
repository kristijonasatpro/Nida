# MIDI discovery during startup

The application starts native MIDI discovery on a worker and polls its result
without waiting. A slow driver no longer delays engine construction or the
first usable control window. This applies to both GUI and headless startup;
manual rescanning of an initialized manager remains synchronous.

When discovery completes, the engine applies the current show's controller
profiles, restores named manual mappings, synchronizes controller feedback
and auto-mapping, and then drains MIDI messages. Profiles are applied to the
discovered ports without a second native scan. Discovery failure is reported
through the existing log and notification system; the show keeps running.

The current MIDI configuration is retained while discovery is pending or
unavailable, so Save, Save As and quit preserve it. Bindings for disconnected
controllers also survive subsequent saves. Connected controllers use the
live mapping store, so removing a binding does not restore its old saved
entry. Clear mappings explicitly persists an empty configuration.

A saved forced MTC port is resolved when devices become available. Changing
the timecode preference while discovery is pending cancels that deferred
choice. Loading another workspace replaces deferred settings with that
workspace's own configuration.

The serialized `midi.json` and `stage.json` formats are unchanged; no saved-show
migration or new API command is required. MIDI controls remain unavailable
until discovery completes. Quitting drops the pending result receiver rather
than joining a worker that may be blocked in a native driver.

Regression tests use detached devices and a deliberately blocked worker to
cover pending saves, delayed adoption, discovery failure/disconnection,
timecode restoration, explicit edits and receiver cancellation. Native
Windows measurements reproduced a slow scan while engine initialization
completed independently. Those measurements do not certify Mac/Linux
drivers or every physical controller.
