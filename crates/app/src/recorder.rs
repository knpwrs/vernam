//! Recording a voice message, as Signal Desktop does (`AudioRecorder`, the
//! `audioRecorder` duck): from the microphone to a file, with the level
//! drawn as it goes, for up to an hour. Desktop encodes MP3; this records
//! AAC in an M4A file, which is what Signal's phones send.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// Desktop's limit (`CompositionRecording`: `DurationInSeconds.HOUR`).
pub const MAX_SECONDS: f64 = 60. * 60.;

/// Levels kept for the bars (Desktop's `MAX_PEAKS`).
pub const MAX_LEVELS: usize = 400;

/// Something recording: the microphone, or a stand-in in tests.
pub trait Recorder {
    /// Seconds recorded so far.
    fn elapsed(&self) -> f64;
    /// How loud it is now, from 0 to 1.
    fn level(&mut self) -> f32;
    /// Stops recording, leaving the file complete.
    fn stop(&mut self);
}

/// Starts recording into `path`.
pub type StartRecorder = fn(&Path) -> anyhow::Result<Box<dyn Recorder>>;

/// A recording in the composer: where it goes, its recorder, and the
/// levels so far (newest last).
pub struct Recording {
    pub path: PathBuf,
    recorder: Box<dyn Recorder>,
    pub levels: VecDeque<f32>,
    /// Stopped at the time limit, waiting to be sent or discarded.
    pub stopped: bool,
}

impl Recording {
    pub fn new(path: PathBuf, recorder: Box<dyn Recorder>) -> Self {
        Self { path, recorder, levels: VecDeque::new(), stopped: false }
    }

    pub fn elapsed(&self) -> f64 {
        self.recorder.elapsed()
    }

    /// Takes a level reading. Returns true when the time limit was just
    /// reached, which stops the recording.
    pub fn tick(&mut self) -> bool {
        if self.stopped {
            return false;
        }
        let level = self.recorder.level();
        if self.levels.len() == MAX_LEVELS {
            self.levels.pop_front();
        }
        self.levels.push_back(level);
        if self.recorder.elapsed() >= MAX_SECONDS {
            self.stop();
            return true;
        }
        false
    }

    pub fn stop(&mut self) {
        if !self.stopped {
            self.recorder.stop();
            self.stopped = true;
        }
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A file to record the next voice message into, never one in use.
pub fn new_path() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("vernam-voice-{}-{millis}-{n}.m4a", std::process::id()))
}

/// Records from the default microphone. macOS asks for permission the first
/// time; if it's refused, recording fails to start.
pub fn start_microphone(path: &Path) -> anyhow::Result<Box<dyn Recorder>> {
    #[cfg(target_os = "macos")]
    {
        Microphone::start(path).map(|m| Box::new(m) as Box<dyn Recorder>)
    }
    #[cfg(target_os = "linux")]
    {
        Microphone::start(path).map(|m| Box::new(m) as Box<dyn Recorder>)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        anyhow::bail!("recording isn't available here")
    }
}

#[cfg(target_os = "macos")]
struct Microphone {
    recorder: objc2::rc::Retained<objc2_avf_audio::AVAudioRecorder>,
    /// `currentTime` reads 0 once stopped; the length is kept from before.
    stopped_at: Option<f64>,
}

#[cfg(target_os = "macos")]
impl Microphone {
    fn start(path: &Path) -> anyhow::Result<Self> {
        use objc2::AnyThread as _;
        use objc2::runtime::AnyObject;
        use objc2_avf_audio::{AVAudioRecorder, AVEncoderBitRateKey, AVFormatIDKey, AVNumberOfChannelsKey, AVSampleRateKey};
        use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};
        let path = path.to_str().ok_or_else(|| anyhow::anyhow!("not a UTF-8 path"))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        // kAudioFormatMPEG4AAC ('aac '); mono speech at 44.1 kHz, 64 kbit/s.
        const MPEG4_AAC: u32 = u32::from_be_bytes(*b"aac ");
        let keys = unsafe { [AVFormatIDKey, AVSampleRateKey, AVNumberOfChannelsKey, AVEncoderBitRateKey] };
        let keys: Vec<&NSString> = keys.into_iter().collect::<Option<_>>().ok_or_else(|| anyhow::anyhow!("no audio settings keys"))?;
        let values = [NSNumber::new_u32(MPEG4_AAC), NSNumber::new_f64(44_100.), NSNumber::new_u32(1), NSNumber::new_u32(64_000)];
        let values: Vec<&AnyObject> = values.iter().map(|v| &**v as &AnyObject).collect();
        let settings = NSDictionary::from_slices(&keys, &values);
        let recorder = unsafe { AVAudioRecorder::initWithURL_settings_error(AVAudioRecorder::alloc(), &url, &settings) }
            .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))?;
        unsafe {
            recorder.setMeteringEnabled(true);
            if !recorder.record() {
                anyhow::bail!("the microphone didn't start (is access to it allowed?)");
            }
        }
        Ok(Self { recorder, stopped_at: None })
    }
}

#[cfg(target_os = "macos")]
impl Recorder for Microphone {
    fn elapsed(&self) -> f64 {
        self.stopped_at.unwrap_or_else(|| unsafe { self.recorder.currentTime() })
    }

    fn level(&mut self) -> f32 {
        unsafe {
            self.recorder.updateMeters();
            // The loudness since the last reading, as Desktop's encoder
            // measures each 100 ms (a mean square, in decibels).
            decibels_to_level(self.recorder.averagePowerForChannel(0))
        }
    }

    fn stop(&mut self) {
        if self.stopped_at.is_none() {
            self.stopped_at = Some(self.elapsed());
            unsafe { self.recorder.stop() };
        }
    }
}

/// Records from the default microphone (rodio, ALSA or PulseAudio) and
/// pipes the samples to ffmpeg, which encodes the same AAC the macOS
/// recorder records: mono speech at 44.1 kHz, 64 kbit/s. The drain thread
/// also measures the loudness of each 100 ms, as Desktop's encoder does.
#[cfg(target_os = "linux")]
struct Microphone {
    /// Read by `elapsed` and `level` while the drain thread writes.
    shared: std::sync::Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The samples stop when recording stops; the length is kept from before.
    stopped_at: Option<f64>,
}

#[cfg(target_os = "linux")]
struct Shared {
    /// Mono frames written so far, the recording's length in frames.
    frames: std::sync::atomic::AtomicU64,
    /// The microphone's rate, frames per second.
    rate: f64,
    /// The latest 100 ms window's mean power, in decibels (-160 for none).
    decibels: std::sync::Mutex<f32>,
    /// The drain thread stops when this clears.
    wanted: std::sync::atomic::AtomicBool,
}

#[cfg(target_os = "linux")]
impl Microphone {
    fn start(path: &Path) -> anyhow::Result<Self> {
        use rodio::microphone::MicrophoneBuilder;
        use rodio::Source as _;
        let mic = MicrophoneBuilder::new()
            .default_device()?
            .default_config()?
            .open_stream()
            .map_err(|error| anyhow::anyhow!("the microphone didn't start: {error}"))?;
        let rate = mic.sample_rate().get();
        let channels = mic.channels().get() as usize;
        // The encoder starts with the recording, so a missing ffmpeg fails
        // the start, as the mic device does, rather than recording into
        // nothing.
        let child = encode_command(rate, path)
            .spawn()
            .map_err(|error| anyhow::anyhow!("the voice recorder didn't start: {error}"))?;
        let shared = std::sync::Arc::new(Shared {
            frames: std::sync::atomic::AtomicU64::new(0),
            rate: f64::from(rate),
            decibels: std::sync::Mutex::new(NOISE_BED - 100.),
            wanted: std::sync::atomic::AtomicBool::new(true),
        });
        let thread = std::thread::Builder::new().name("voice-record".into()).spawn({
            let shared = shared.clone();
            let path = path.to_owned();
            move || drain(mic, rate, channels, shared, path, child)
        })?;
        Ok(Self { shared, thread: Some(thread), stopped_at: None })
    }
}

#[cfg(target_os = "linux")]
impl Recorder for Microphone {
    fn elapsed(&self) -> f64 {
        self.stopped_at.unwrap_or_else(|| {
            self.shared.frames.load(std::sync::atomic::Ordering::Relaxed) as f64 / self.shared.rate
        })
    }

    fn level(&mut self) -> f32 {
        decibels_to_level(*self.shared.decibels.lock().unwrap())
    }

    fn stop(&mut self) {
        if self.stopped_at.is_none() {
            self.stopped_at = Some(self.elapsed());
            self.shared.wanted.store(false, std::sync::atomic::Ordering::Relaxed);
            // The thread writes what it has, waits for ffmpeg, and cleans up.
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

/// Drains the microphone into ffmpeg until stopped. Nothing is buffered
/// here beyond one frame: what the device delivers, ffmpeg gets.
#[cfg(target_os = "linux")]
fn drain(
    mic: rodio::microphone::Microphone,
    rate: u32,
    channels: usize,
    shared: std::sync::Arc<Shared>,
    path: std::path::PathBuf,
    mut child: std::process::Child,
) {
    use std::io::Write as _;
    let mut meter = Meter::new(rate);
    let mut stdin = child.stdin.take().expect("ffmpeg's stdin is piped");
    let mut frame: Vec<f32> = Vec::with_capacity(channels);
    let mut pending: Vec<u8> = Vec::with_capacity(8192);
    for sample in mic {
        if !shared.wanted.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        frame.push(sample);
        if frame.len() != channels {
            continue;
        }
        shared.frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        meter.push(&frame, &mut shared.decibels.lock().unwrap());
        pending.extend_from_slice(&mono_s16(&frame).to_le_bytes());
        frame.clear();
        if pending.len() == pending.capacity() {
            if stdin.write_all(&pending).is_err() {
                break; // ffmpeg is gone; what it left is cleaned up below.
            }
            pending.clear();
        }
    }
    let _ = stdin.write_all(&pending);
    drop(stdin);
    match child.wait_with_output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            tracing::warn!(
                "a voice recording didn't encode: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            let _ = std::fs::remove_file(&path);
        }
        Err(error) => tracing::warn!("a voice recording didn't finish: {error}"),
    }
}

/// ffmpeg reading our raw mono samples from stdin and writing the AAC
/// voice message, the same encode the macOS recorder makes.
#[cfg(target_os = "linux")]
fn encode_command(rate: u32, out: &Path) -> std::process::Command {
    use std::process::Stdio;
    let mut command = std::process::Command::new(crate::video::tool("ffmpeg"));
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "s16le",
            "-ar",
            &rate.to_string(),
            "-ac",
            "1",
            "-i",
            "pipe:0",
            "-c:a",
            "aac",
            "-b:a",
            "64k",
            "-ar",
            "44100",
            "-ac",
            "1",
        ])
        .arg(out)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// The loudness meter: a running mean square over 100 ms of frames,
/// reported in decibels as each window fills.
#[cfg(target_os = "linux")]
struct Meter {
    window_len: u64,
    sum: f64,
    frames: u64,
}

#[cfg(target_os = "linux")]
impl Meter {
    fn new(rate: u32) -> Self {
        Self { window_len: (u64::from(rate) / 10).max(1), sum: 0., frames: 0 }
    }

    fn push(&mut self, frame: &[f32], decibels: &mut f32) {
        let mean_square = frame.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / frame.len() as f64;
        self.sum += mean_square;
        self.frames += 1;
        if self.frames == self.window_len {
            *decibels = (10. * (self.sum / self.frames as f64).log10()) as f32;
            self.sum = 0.;
            self.frames = 0;
        }
    }
}

/// Mixes a frame down to one signed 16-bit sample, clamped (a broken
/// device can deliver out-of-range floats).
#[cfg(target_os = "linux")]
fn mono_s16(frame: &[f32]) -> i16 {
    let mixed = frame.iter().sum::<f32>() / frame.len() as f32;
    (mixed.clamp(-1., 1.) * f32::from(i16::MAX)) as i16
}

/// Below this, a bar is empty (Desktop's `NOISE_BED`).
const NOISE_BED: f32 = -60.;

/// A meter reading (decibels below full scale, -160 for silence) as a bar's
/// height from 0 to 1: Desktop's `toPeak` (`ts/util/waveformBuilder`),
/// linear in decibels from the noise bed to full scale, so speech fills
/// half the bar or more rather than a few percent.
pub fn decibels_to_level(decibels: f32) -> f32 {
    if decibels.is_finite() { ((decibels - NOISE_BED) / -NOISE_BED).clamp(0., 1.) } else { 0. }
}

#[cfg(test)]
pub mod fake {
    //! A recorder that records nothing: time and levels are set by the test.

    use std::cell::Cell;
    use std::rc::Rc;

    use super::Recorder;

    #[derive(Clone, Default)]
    pub struct FakeMic {
        pub seconds: Rc<Cell<f64>>,
        pub level: Rc<Cell<f32>>,
        pub stopped: Rc<Cell<bool>>,
    }

    impl Recorder for FakeMic {
        fn elapsed(&self) -> f64 {
            self.seconds.get()
        }

        fn level(&mut self) -> f32 {
            self.level.get()
        }

        fn stop(&mut self) {
            self.stopped.set(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeMic;
    use super::*;

    #[test]
    fn levels_are_kept_newest_last_up_to_the_limit() {
        let mic = FakeMic::default();
        let mut recording = Recording::new(PathBuf::from("x.m4a"), Box::new(mic.clone()));
        for i in 0..MAX_LEVELS + 5 {
            mic.level.set(i as f32);
            assert!(!recording.tick());
        }
        assert_eq!(recording.levels.len(), MAX_LEVELS);
        assert_eq!(recording.levels.back(), Some(&((MAX_LEVELS + 4) as f32)));
    }

    #[test]
    fn an_hour_stops_it() {
        let mic = FakeMic::default();
        let mut recording = Recording::new(PathBuf::from("x.m4a"), Box::new(mic.clone()));
        mic.seconds.set(MAX_SECONDS - 1.);
        assert!(!recording.tick());
        mic.seconds.set(MAX_SECONDS);
        assert!(recording.tick(), "the limit");
        assert!(mic.stopped.get() && recording.stopped);
        assert!(!recording.tick(), "only once");
    }

    #[test]
    fn dropping_it_stops_the_microphone() {
        let mic = FakeMic::default();
        drop(Recording::new(PathBuf::from("x.m4a"), Box::new(mic.clone())));
        assert!(mic.stopped.get());
    }

    #[test]
    fn meter_readings_become_levels() {
        assert_eq!(decibels_to_level(0.), 1.);
        assert!((decibels_to_level(-20.) - 2. / 3.).abs() < 1e-6, "speech is most of the bar");
        assert_eq!(decibels_to_level(-60.), 0., "Desktop's noise bed");
        assert_eq!(decibels_to_level(-160.), 0., "silence");
        assert_eq!(decibels_to_level(f32::NEG_INFINITY), 0.);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_meter_reports_each_100_ms_window() {
        let mut decibels = -160.;
        let mut meter = Meter::new(8000);
        assert_eq!(meter.window_len, 800);
        // Full scale for exactly one window reads 0 decibels.
        for _ in 0..meter.window_len {
            meter.push(&[1., -1.], &mut decibels);
        }
        assert!((decibels - 0.).abs() < 1e-9, "{decibels}");
        assert_eq!(decibels_to_level(decibels), 1.);
        // Silence reads minus infinity, an empty bar.
        for _ in 0..meter.window_len {
            meter.push(&[0., 0.], &mut decibels);
        }
        assert_eq!(decibels_to_level(decibels), 0.);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn frames_mix_down_to_clamped_sixteen_bit_samples() {
        assert_eq!(mono_s16(&[0.5, -0.5]), 0);
        assert_eq!(mono_s16(&[1., 1.]), i16::MAX);
        assert_eq!(mono_s16(&[-1., -1.]), -i16::MAX);
        assert_eq!(mono_s16(&[2., 2.]), i16::MAX, "clamped");
    }

    /// The same command the drain thread runs, fed raw samples by a file:
    /// what comes out must be a valid M4A of the recorded length.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_encode_command_makes_a_valid_m4a() {
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("skipping: no ffmpeg");
            return;
        }
        // Half a second of a 440 Hz sine at 48 kHz, as the mic delivers it.
        let rate = 48_000;
        let samples: Vec<i16> = (0..rate / 2)
            .map(|n| ((2. * std::f64::consts::PI * 440. * n as f64 / rate as f64).sin() * 8000.) as i16)
            .collect();
        let mut raw = Vec::with_capacity(samples.len() * 2);
        for sample in &samples {
            raw.extend_from_slice(&sample.to_le_bytes());
        }
        let raw_path = std::env::temp_dir().join(format!("vernam-voice-test-{}-in.pcm", std::process::id()));
        std::fs::write(&raw_path, &raw).unwrap();
        let out_path = std::env::temp_dir().join(format!("vernam-voice-test-{}-out.m4a", std::process::id()));
        let mut child = encode_command(rate, &out_path).spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), &raw).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let (samples, _, decoded_rate) = crate::audio::linux::decode_for_playback(&out_path).unwrap();
        assert_eq!(decoded_rate, 48_000);
        let seconds = samples.len() as f64 / decoded_rate as f64 / 2.;
        assert!((seconds - 0.5).abs() < 0.05, "{seconds}");
        drop(std::fs::remove_file(raw_path));
        drop(std::fs::remove_file(out_path));
    }
}
