//! Audio: a thin, robust wrapper over `rodio`.
//!
//! Design notes:
//! * Failure to open an output device (headless CI, no sound server) is
//!   *not* fatal — the engine logs once and continues silently. This keeps
//!   the engine testable on GPU-less, sound-less machines.
//! * Sound bytes are cached by the asset system; this module only plays.
//! * One-shot sounds detach from their sink (fire-and-forget); music keeps
//!   its sink and re-appends the decoder when it drains (looping).

use std::io::Cursor;

pub struct Audio {
    _stream: Option<rodio::OutputStream>,
    mixer: Option<rodio::mixer::Mixer>,
    music: Option<rodio::Sink>,
    music_bytes: Vec<u8>,
    music_volume: f32,
    warned: bool,
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

impl Audio {
    /// Connect to the default output device; degrade gracefully if absent.
    pub fn new() -> Self {
        let stream = rodio::OutputStreamBuilder::open_default_stream().ok();
        let mixer = stream.as_ref().map(|s| s.mixer().clone());
        if mixer.is_none() {
            log::info!("audio: no output device, running silent");
        }
        Self {
            _stream: stream,
            mixer,
            music: None,
            music_bytes: Vec::new(),
            music_volume: 0.6,
            warned: false,
        }
    }

    pub fn available(&self) -> bool {
        self.mixer.is_some()
    }

    /// Play a one-shot sound effect from cached bytes (wav/ogg/mp3/flac).
    pub fn play_bytes(&mut self, bytes: &[u8], volume: f32) {
        let Some(mixer) = &self.mixer else {
            self.warn_once();
            return;
        };
        if let Ok(sink) = rodio::play(mixer, Cursor::new(bytes.to_vec())) {
            sink.set_volume(volume.clamp(0.0, 2.0));
            sink.detach();
        } else {
            log::warn!("audio: could not decode sound (unsupported format?)");
        }
    }

    /// Start (or switch) looping background music.
    pub fn play_music(&mut self, bytes: &[u8], volume: f32) {
        let Some(mixer) = &self.mixer else {
            self.warn_once();
            return;
        };
        let sink = rodio::Sink::connect_new(mixer);
        if let Ok(decoder) = rodio::Decoder::new(Cursor::new(bytes.to_vec())) {
            sink.set_volume(volume.clamp(0.0, 2.0));
            sink.append(decoder);
            self.music = Some(sink);
            self.music_bytes = bytes.to_vec();
            self.music_volume = volume;
        }
    }

    pub fn stop_music(&mut self) {
        if let Some(sink) = self.music.take() {
            sink.stop();
        }
        self.music_bytes.clear();
    }

    pub fn set_music_volume(&mut self, volume: f32) {
        self.music_volume = volume;
        if let Some(sink) = &self.music {
            sink.set_volume(volume.clamp(0.0, 2.0));
        }
    }

    /// Housekeeping: loop drained music.
    pub fn update(&mut self) {
        let drained = self.music.as_ref().map(|s| s.empty()).unwrap_or(false);
        if drained && !self.music_bytes.is_empty() {
            let bytes = std::mem::take(&mut self.music_bytes);
            let vol = self.music_volume;
            self.play_music(&bytes, vol);
        }
    }

    fn warn_once(&mut self) {
        if !self.warned {
            log::warn!("audio: no output device available; sounds will be skipped");
            self.warned = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Must not panic even with no audio device (CI runners).
    #[test]
    fn headless_safe() {
        let mut a = Audio::new();
        a.play_bytes(&[], 0.5);
        a.play_music(&[], 0.5);
        a.stop_music();
        a.update();
    }
}
