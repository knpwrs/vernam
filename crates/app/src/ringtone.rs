//! Signal Desktop's ringtone (`sounds/ringtone_minimal.ogg`, AGPL-3.0-only
//! like this app; converted to AAC, which macOS plays), looped while a call
//! rings.

/// The ringtone, playing until dropped.
pub struct Ringtone {
    #[cfg(target_os = "macos")]
    sound: objc2::rc::Retained<objc2_app_kit::NSSound>,
    /// Kept for its drop: dropping it stops the sound. (The lint can't see
    /// that a drop is the point.)
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    player: rodio::Player,
}

#[cfg(not(all(target_os = "linux", test)))]
const RINGTONE: &[u8] = include_bytes!("../assets/sounds/ringtone.m4a");

impl Ringtone {
    /// Starts ringing; `None` where sound isn't available, and in tests,
    /// which never play sound.
    pub fn start() -> Option<Self> {
        #[cfg(test)]
        return None;
        #[cfg(all(target_os = "macos", not(test)))]
        {
            use objc2::AnyThread as _;
            use objc2_app_kit::NSSound;
            use objc2_foundation::NSData;
            let data = NSData::with_bytes(RINGTONE);
            let sound = NSSound::initWithData(NSSound::alloc(), &data)?;
            sound.setLoops(true);
            sound.play();
            Some(Self { sound })
        }
        #[cfg(all(target_os = "linux", not(test)))]
        {
            use std::num::NonZero;
            use std::sync::Mutex;

            use rodio::source::Source as _;
            // Decoded once, then looped through the device. A failed decode
            // isn't kept, as with the device.
            static SAMPLES: Mutex<Option<&'static (Vec<f32>, u16, u32)>> = Mutex::new(None);
            let decoded = {
                let mut samples = SAMPLES.lock().unwrap_or_else(|e| e.into_inner());
                match *samples {
                    Some(decoded) => Some(decoded),
                    None => {
                        match crate::audio::linux::with_temp_file("ringtone", RINGTONE, crate::audio::linux::decode_for_playback) {
                            Ok(Ok(decoded)) => {
                                *samples = Some(&*Box::leak(Box::new(decoded)));
                                *samples
                            }
                            _ => None,
                        }
                    }
                }
            };
            let (samples, channels, rate) = decoded?;
            let device = crate::audio::linux::device().ok()?;
            let source = rodio::static_buffer::StaticSamplesBuffer::new(
                NonZero::new(*channels).unwrap(),
                NonZero::new(*rate).unwrap(),
                samples,
            )
            .repeat_infinite();
            let uniform = rodio::source::UniformSourceIterator::new(
                source,
                device.config().channel_count(),
                device.config().sample_rate(),
            );
            let player = rodio::Player::connect_new(device.mixer());
            player.append(uniform);
            Some(Self { player })
        }
    }
}

impl Drop for Ringtone {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        self.sound.stop();
        // Linux's player stops in its own drop.
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn macos_can_play_the_ringtone() {
        use objc2::AnyThread as _;
        use objc2_app_kit::NSSound;
        let data = objc2_foundation::NSData::with_bytes(super::RINGTONE);
        let sound = NSSound::initWithData(NSSound::alloc(), &data).expect("decodable");
        assert!(sound.duration() > 7.0, "{}", sound.duration());
    }
}
