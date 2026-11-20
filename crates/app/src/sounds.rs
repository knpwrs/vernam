//! Signal Desktop's in-chat sounds (`ts/util/Sound.std.ts`): `pop` when a
//! message arrives in the open chat, `whoosh` when we send one, and
//! `state-change_confirm-up` when consecutive voice messages run out.
//! Desktop's `sounds/*.ogg` (AGPL-3.0-only like this app), converted to
//! AAC, which macOS plays. Tests never play sound.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Pop,
    Whoosh,
    /// The end of consecutive voice messages (Desktop's
    /// `state-change_confirm-up`).
    ConfirmUp,
}

const POP: &[u8] = include_bytes!("../assets/sounds/pop.m4a");
const WHOOSH: &[u8] = include_bytes!("../assets/sounds/whoosh.m4a");
const CONFIRM_UP: &[u8] = include_bytes!("../assets/sounds/confirm-up.m4a");

impl Sound {
    fn bytes(self) -> &'static [u8] {
        match self {
            Sound::Pop => POP,
            Sound::Whoosh => WHOOSH,
            Sound::ConfirmUp => CONFIRM_UP,
        }
    }
}

/// Plays `sound` once, on the main thread.
pub fn play(sound: Sound) {
    #[cfg(all(target_os = "macos", not(test)))]
    macos::play(sound.bytes());
    #[cfg(all(target_os = "linux", not(test)))]
    linux::play(sound);
    #[cfg(any(not(any(target_os = "macos", target_os = "linux")), test))]
    let _ = sound.bytes();
}

#[cfg(all(target_os = "macos", not(test)))]
mod macos {
    use std::cell::RefCell;

    use objc2::AnyThread as _;
    use objc2::rc::Retained;
    use objc2_app_kit::NSSound;
    use objc2_foundation::NSData;

    thread_local! {
        /// The sounds playing, kept until they're done.
        static PLAYING: RefCell<Vec<Retained<NSSound>>> = const { RefCell::new(Vec::new()) };
    }

    pub fn play(bytes: &'static [u8]) {
        let data = NSData::with_bytes(bytes);
        let Some(sound) = NSSound::initWithData(NSSound::alloc(), &data) else { return };
        sound.play();
        PLAYING.with(|playing| {
            let mut playing = playing.borrow_mut();
            playing.retain(|s| s.isPlaying());
            playing.push(sound);
        });
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn macos_can_decode_the_sounds() {
        use objc2::AnyThread as _;
        use objc2_app_kit::NSSound;
        for sound in [super::Sound::Pop, super::Sound::Whoosh, super::Sound::ConfirmUp] {
            let data = objc2_foundation::NSData::with_bytes(sound.bytes());
            let decoded = NSSound::initWithData(NSSound::alloc(), &data).expect("decodable");
            assert!(decoded.duration() > 0.05, "{sound:?}: {}", decoded.duration());
        }
    }
}

/// ffmpeg decodes each sound once, to the fixed playback format; rodio's
/// uniform converter adapts it to the device.
#[cfg(all(target_os = "linux", not(test)))]
mod linux {
    use std::num::NonZero;
    use std::sync::Mutex;

    use super::Sound;
    use crate::audio::linux::{decode_for_playback, device, with_temp_file};

    /// The decoded sounds, in `Sound`'s order.
    fn decoded(sound: Sound) -> Option<&'static (Vec<f32>, u16, u32)> {
        // A failed decode isn't kept, as with the device: ffmpeg can
        // appear mid-session, and the temp dir can recover.
        static DECODED: Mutex<[Option<&'static (Vec<f32>, u16, u32)>; 3]> = Mutex::new([None; 3]);
        if let Some(decoded) = DECODED.lock().unwrap_or_else(|e| e.into_inner())[sound as usize] {
            return Some(decoded);
        }
        let decoded = match with_temp_file("sound", sound.bytes(), decode_for_playback) {
            Ok(Ok(decoded)) => &*Box::leak(Box::new(decoded)),
            _ => return None,
        };
        DECODED.lock().unwrap_or_else(|e| e.into_inner())[sound as usize] = Some(decoded);
        Some(decoded)
    }

    /// The sounds playing, kept until they're done.
    static PLAYING: Mutex<Vec<rodio::Player>> = Mutex::new(Vec::new());

    pub fn play(sound: Sound) {
        let Some((samples, channels, rate)) = decoded(sound) else { return };
        let Ok(device) = device() else { return };
        let source = rodio::static_buffer::StaticSamplesBuffer::new(
            NonZero::new(*channels).unwrap(),
            NonZero::new(*rate).unwrap(),
            samples,
        );
        let uniform = rodio::source::UniformSourceIterator::new(
            source,
            device.config().channel_count(),
            device.config().sample_rate(),
        );
        let player = rodio::Player::connect_new(device.mixer());
        player.append(uniform);
        let mut playing = PLAYING.lock().unwrap_or_else(|e| e.into_inner());
        playing.retain(|player| !player.empty());
        playing.push(player);
    }
}

/// The sounds decode, and each is more than a blink.
#[cfg(all(test, target_os = "linux"))]
#[test]
fn ffmpeg_can_decode_the_sounds() {
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    for sound in [Sound::Pop, Sound::Whoosh, Sound::ConfirmUp] {
        let (samples, _, rate) = crate::audio::linux::with_temp_file("sound", sound.bytes(), |path| {
            crate::audio::linux::decode_for_playback(path)
        })
        .unwrap()
        .unwrap();
        assert!(!samples.is_empty(), "{sound:?}");
        assert!(samples.len() as f64 / rate as f64 > 0.05, "{sound:?}");
    }
}
