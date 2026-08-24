//! Audio workflow tests: the Music component must actually drive playback
//! bookkeeping (track selection, volume follow, loop state) through the
//! real Engine tick loop — headless (no device) but with a real decoded
//! WAV on disk so the asset path resolves.

use spark::app::Engine;
use spark::components::{Camera, Music, Transform};
use spark::ecs;

/// Write a minimal but valid 16-bit PCM WAV (~0.1 s of 440 Hz).
fn write_wav(path: &std::path::Path) {
    let sample_rate: u32 = 8000;
    let samples: u32 = 800; // 0.1 s
    let data_len = samples * 2;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..samples {
        let t = i as f32 / sample_rate as f32;
        let v = (t * 440.0 * std::f32::consts::TAU).sin() * 0.5;
        wav.extend_from_slice(&(v as i16).to_le_bytes());
    }
    std::fs::write(path, wav).unwrap();
}

fn engine_with_music(dir: &std::path::Path) -> Engine<'static> {
    // Minimal project manifest so Engine::headless loads a main scene.
    std::fs::write(
        dir.join("project.ron"),
        "(name: \"AudioTest\", dimension: D2, main_scene: \"scenes/main.scene\")",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("scenes")).unwrap();
    std::fs::write(dir.join("scenes/main.scene"), "(name: \"Main\")").unwrap();
    let mut e = Engine::headless(dir).expect("headless engine boots");
    e.scene.world.spawn((
        ecs::Name("Camera".into()),
        Transform::default(),
        Camera::default(),
    ));
    e
}

/// The Music component's track autoplay starts on the first tick and the
/// engine records it as playing.
#[test]
fn music_component_autoplays() {
    let dir = std::env::temp_dir().join(format!("spark_audio_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    write_wav(&dir.join("assets/theme.wav"));

    let mut e = engine_with_music(&dir);
    e.scene.world.spawn((
        ecs::Name("Music".into()),
        Transform::default(),
        Music {
            track: "assets/theme.wav".into(),
            volume: 0.7,
        },
    ));
    assert!(e.playing_track.is_none(), "nothing before the first tick");
    e.tick(1.0 / 60.0);
    assert_eq!(
        e.playing_track.as_deref(),
        Some("assets/theme.wav"),
        "the Music component must autoplay its track"
    );
    assert!((e.music_volume - 0.7).abs() < 1e-4);

    // Removing the Music component stops it.
    let music_e = ecs::find_by_name(&e.scene.world, "Music").unwrap();
    e.scene.world.remove_one::<Music>(music_e).ok();
    e.tick(1.0 / 60.0);
    assert!(
        e.playing_track.is_none(),
        "music stops when the component goes"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Volume edits mid-play are followed live (no restart needed).
#[test]
fn music_volume_follows_live_edits() {
    let dir = std::env::temp_dir().join(format!("spark_audio_vol_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    write_wav(&dir.join("assets/theme.wav"));

    let mut e = engine_with_music(&dir);
    let m = e.scene.world.spawn((
        ecs::Name("Music".into()),
        Transform::default(),
        Music {
            track: "assets/theme.wav".into(),
            volume: 0.5,
        },
    ));
    e.tick(1.0 / 60.0);
    assert!((e.music_volume - 0.5).abs() < 1e-4);

    // User drags the volume in the Inspector while playing.
    if let Ok(mut music) = e.scene.world.get::<&mut Music>(m) {
        music.volume = 0.9;
    }
    e.tick(1.0 / 60.0);
    assert!(
        (e.music_volume - 0.9).abs() < 1e-4,
        "volume edits must apply without stopping the track"
    );
    // The track itself was never switched.
    assert_eq!(e.playing_track.as_deref(), Some("assets/theme.wav"));
    std::fs::remove_dir_all(&dir).ok();
}

/// A missing track file must not panic or wedge the autoplay state.
#[test]
fn missing_track_degrades_gracefully() {
    let dir = std::env::temp_dir().join(format!("spark_audio_missing_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assets")).unwrap();

    let mut e = engine_with_music(&dir);
    e.scene.world.spawn((
        ecs::Name("Music".into()),
        Transform::default(),
        Music {
            track: "assets/does-not-exist.wav".into(),
            volume: 0.5,
        },
    ));
    for _ in 0..10 {
        e.tick(1.0 / 60.0);
    }
    assert!(
        e.playing_track.is_none(),
        "missing file → no track, no panic"
    );
    std::fs::remove_dir_all(&dir).ok();
}
