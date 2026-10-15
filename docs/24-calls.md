# Zoom and Teams Calls

A remote speaker joins the show over Zoom or Microsoft Teams: their camera full
screen, their voice in the mix, their slides as an input of their own — and
Nida's programme going back to the call as a camera and a microphone, so they
see and hear the room. The guest just joins the call as usual. Nothing is
needed on Zoom's or Teams' side: no add-on plan, no admin setting.

The call runs in the ordinary Zoom or Teams app, on the Mac that runs Nida.

## The call into Nida

- **Their sound** — in the Audio Mixer, **+ Input → App sound →** the call
  app (*zoom.us*, *Microsoft Teams*). It becomes a strip like a microphone,
  with its fader, sends and delay, taken straight from the app (see
  [Audio Mixer → Inputs](16-audio-mixer.md#inputs)). macOS asks once whether
  Nida may record other apps' sound. The app goes quiet on the Mac's own
  speakers while Nida takes it, so the room hears the call once, through
  Nida.
- **Their camera** — in the call app, show the speaker alone in a large
  window: pin or spotlight them, speaker view, self view hidden. Then **Add
  Input → Desktop Capture →** that window
  ([Desktop capture](23-adding-inputs.md#desktop-capture)), and trim the
  window's edges with **Crop**.
- **Their slides** — have the call app show a shared screen in a window of
  its own (Zoom: *Use dual monitors* in its settings; Teams: pop the shared
  content out), and capture that window as another input.

## Nida back to the call

Nida's own camera and microphone for calls need an Apple Developer signature
and are not there yet. (OBS's virtual camera is a camera extension its makers
sign with theirs.) Until then, the picture goes back one of three ways:

1. **Over NDI** — NDI's free **NDI Virtual Input** (in NDI Tools for Mac)
   makes Nida's NDI output a camera the call app can choose. The NDI output
   alone adds no camera:
   - In **Outputs**, an NDI output ([NDI](09-streaming-and-io.md#ndi))
     showing Main or a bus, started (**▶ Start**): a new output is stopped.
   - Install NDI Tools and start **NDI Virtual Input**. The first time, macOS
     asks to allow its camera extension. The dialog's OK does not do it: turn
     the extension on in System Settings → General → Login Items & Extensions
     → Camera Extensions, then quit NDI Virtual Input and start it again.
     Only then do call apps list its camera.
   - In NDI Virtual Input, choose Nida's NDI output; in the call app, choose
     its camera. It carries one source at a time: one output to one call.
     Once chosen, NDI Virtual Input can be quit: the camera keeps showing
     Nida's output, as the extension receives it by itself.
   - Sound: if NDI Virtual Input adds a microphone on your Mac too, give the
     NDI output's **Audio:** the call's bus (next section) and choose that
     microphone in the call app. NDI documents only its camera on a Mac, so
     check it in rehearsal. Otherwise use a virtual audio cable (below).
2. **Through OBS** — if OBS's **Virtual Camera** already shows in the call
   app, add a **+ Windowed** output ([Outputs](07-outputs.md#creating-an-output))
   showing Main, capture that window in OBS (macOS Screen Capture, the
   window) and **Start Virtual Camera** in OBS. Keep the window open, not
   minimised; OBS adds a little delay. OBS's camera carries no sound: use a
   virtual audio cable (below).
3. **Over HDMI** — an output on a spare HDMI port
   ([Outputs](07-outputs.md#output-targets)) into a USB capture device the Mac
   takes as a webcam. Choose it as the call's camera. For the sound, make the
   call's bus play on that screen's sound (its **Plays on**) and choose the
   capture device as the call app's microphone — if it passes the HDMI's
   sound over USB, as a capture card does. A switcher's USB webcam output
   carries the switcher's own programme and sound mix instead: put Nida's
   input on its programme with only that input's sound up, or the guest hears
   the switcher's mix.

**Virtual audio cable** — play the call's bus on **BlackHole 2ch** (free,
installed once with the Mac's password) by picking it as that bus's **Plays
on**, and choose BlackHole as the call app's microphone.

## The call must not hear itself

Make a bus for the call (**+ Bus**, name it *CALL*) and send it everything the
remote speaker should hear — the room's microphones, clips — but not the
call's own sound. That bus is what goes back to the call (above). The call's
sound goes to Master (and the room, stream and recording buses) as usual.
Otherwise the guest hears themselves back a moment late.

## Before the show

- Rehearse with the same Zoom or Teams version, on the same Mac: picture,
  both directions of sound, and the slides window.
- Turn the call app's noise suppression down for music (Zoom: *original
  sound*; Teams: noise suppression off), or clips' music is cut away.
- Leave the call app (and OBS, for that route) running; quitting the call
  app stops its sound until it starts again (Nida takes it again by
  itself).
- An app's sound strip that says **not taken** — the app runs, but its sound
  could not be taken: its tooltip says why. Usually there is no sound output
  without inputs of its own to clock it (a Mac with only a USB interface): set
  the Mac's output to its speakers or HDMI.
- A strip whose meter stays flat while the call has sound: perhaps macOS was
  told no when it asked. Check that Nida is allowed under System Settings →
  Privacy & Security → Screen & System Audio Recording, then quit and open
  Nida again.

## Over the API

`POST /api/audio-inputs/app` with `{"app": "us.zoom.xos"}` adds a call app's
sound (the bundle IDs are listed under `audio_mix.apps` in `GET /api/state`);
`{"name": "Guest"}` names the strip.
