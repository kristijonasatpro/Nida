//! A sound file on a deck — music, a jingle, a voice-over.
//!
//! It plays like a clip: the deck has a playhead that plays, pauses, loops and
//! seeks, and the soundtrack follows it exactly as a video's does (see
//! `audio_mix::clip`). Its picture is transparent, so it can sit on a channel
//! as a layer and add sound without covering anything, as vMix's audio
//! inputs do.

use std::path::Path;

use anyhow::{Context as _, Result};
use ffmpeg_next as ffmpeg;

use super::{FramePlayer, PlaybackState};

/// Side of the transparent picture a sound file shows.
pub const SOUND_PICTURE_SIZE: u32 = 2;
/// How often the playhead moves on. Nothing is decoded, so it costs nothing.
const TICK_FPS: f64 = 30.0;

/// Whether `path` holds sound and no moving picture. A cover picture (the
/// album art in an MP3) is not a picture to show.
pub fn is_sound_only(path: &Path) -> bool {
    if ffmpeg::init().is_err() {
        return false;
    }
    let Ok(ictx) = ffmpeg::format::input(&path) else {
        return false;
    };
    let mut sound = false;
    for stream in ictx.streams() {
        match stream.parameters().medium() {
            ffmpeg::media::Type::Video
                if !stream
                    .disposition()
                    .contains(ffmpeg::format::stream::Disposition::ATTACHED_PIC) =>
            {
                return false;
            }
            ffmpeg::media::Type::Audio => sound = true,
            _ => {}
        }
    }
    sound
}

/// The playhead of a sound file, with a transparent picture.
pub struct SoundOnlyPlayer {
    pub playback: PlaybackState,
    picture: Vec<u8>,
    shown: bool,
}

impl SoundOnlyPlayer {
    /// Open a sound file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or has no sound.
    pub fn open(path: &Path) -> Result<Self> {
        ffmpeg::init().context("Failed to initialize FFmpeg")?;
        let ictx = ffmpeg::format::input(&path).context("Failed to open the sound file")?;
        let stream = ictx
            .streams()
            .best(ffmpeg::media::Type::Audio)
            .context("No sound in the file")?;
        let duration = if ictx.duration() > 0 {
            ictx.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
        } else {
            let tb = stream.time_base();
            stream.duration().max(0) as f64 * f64::from(tb.0) / f64::from(tb.1.max(1))
        };
        anyhow::ensure!(duration > 0.0, "The sound file's length is unknown");
        log::info!("Loaded sound: {duration:.2}s");
        let side = SOUND_PICTURE_SIZE as usize;
        Ok(Self {
            playback: PlaybackState::new(duration, TICK_FPS),
            picture: vec![0; side * side * 4],
            shown: false,
        })
    }
}

impl FramePlayer for SoundOnlyPlayer {
    fn playback(&mut self) -> &mut PlaybackState {
        &mut self.playback
    }

    fn playback_state(&self) -> &PlaybackState {
        &self.playback
    }

    fn next_frame(&mut self) -> Result<Option<&[u8]>> {
        if self.playback.suspended {
            return Ok(None);
        }
        // Moves the playhead: loops, one-shots and in/out points are the
        // playback state's, as for a clip.
        self.playback.advance_frame();
        if self.shown {
            return Ok(None);
        }
        self.shown = true;
        Ok(Some(&self.picture))
    }

    fn seek_and_reset(&mut self, time_secs: f64) -> Result<()> {
        self.playback.position = time_secs.clamp(0.0, self.playback.duration);
        self.playback.reverse = false;
        self.playback.seeks = self.playback.seeks.wrapping_add(1);
        Ok(())
    }

    fn width(&self) -> u32 {
        SOUND_PICTURE_SIZE
    }

    fn height(&self) -> u32 {
        SOUND_PICTURE_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(dir: &Path, name: &str, extra: &[&str]) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut args = vec![
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
        ];
        args.extend_from_slice(extra);
        args.push(path.to_str().unwrap());
        let ok = std::process::Command::new("ffmpeg")
            .args(&args)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "ffmpeg could not make {name}");
        path
    }

    #[test]
    fn a_sound_file_is_sound_only_and_a_video_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let wav = tone(dir.path(), "tone.wav", &[]);
        assert!(is_sound_only(&wav));
        let video = dir.path().join("clip.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x64:rate=10:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=duration=1",
                "-shortest",
                video.to_str().unwrap(),
            ])
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok);
        assert!(!is_sound_only(&video));
    }

    #[test]
    fn an_mp3_with_a_cover_picture_is_still_sound_only() {
        let dir = tempfile::tempdir().unwrap();
        let cover = dir.path().join("cover.png");
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:size=32x32",
                "-frames:v",
                "1",
                cover.to_str().unwrap(),
            ])
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok);
        let mp3 = dir.path().join("song.mp3");
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=duration=2",
                "-i",
                cover.to_str().unwrap(),
                "-map",
                "0",
                "-map",
                "1",
                "-c:v",
                "png",
                "-disposition:v",
                "attached_pic",
                mp3.to_str().unwrap(),
            ])
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok);
        assert!(is_sound_only(&mp3));
    }

    #[test]
    fn the_playhead_runs_only_while_playing_and_shows_one_clear_picture() {
        let dir = tempfile::tempdir().unwrap();
        let wav = tone(dir.path(), "tone.wav", &[]);
        let mut player = SoundOnlyPlayer::open(&wav).unwrap();
        assert!((player.playback.duration - 2.0).abs() < 0.05);
        let picture = player.next_frame().unwrap().expect("the picture").to_vec();
        assert!(picture.iter().all(|&b| b == 0), "transparent");
        assert!(player.next_frame().unwrap().is_none(), "shown once");

        player.playback.playing = false;
        std::thread::sleep(std::time::Duration::from_millis(60));
        player.next_frame().unwrap();
        let parked = player.playback.position;
        std::thread::sleep(std::time::Duration::from_millis(60));
        player.next_frame().unwrap();
        assert!((player.playback.position - parked).abs() < 1e-9, "paused");

        player.playback.playing = true;
        player.next_frame().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(80));
        player.next_frame().unwrap();
        assert!(player.playback.position > parked + 0.05, "playing");

        player.seek_and_reset(1.5).unwrap();
        assert!((player.playback.position - 1.5).abs() < 1e-9);
    }
}
