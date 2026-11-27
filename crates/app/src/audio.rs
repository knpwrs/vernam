//! Audio attachments and voice messages, played as Signal Desktop plays them
//! (`MessageAudio`): one at a time, at 1×, 1.5×, 2× or 0.5×, drawn as a
//! waveform of 47 bars. AVFoundation decodes AAC (what phones send), MP3
//! (what Desktop sends), WAV and the rest macOS knows.

use std::path::Path;

/// Bars in a waveform (Desktop's `BAR_COUNT`).
pub const BARS: usize = 47;

/// Desktop's playback rates, in the order its button steps through them.
pub const RATES: [f32; 4] = [1., 1.5, 2., 0.5];

/// The rate after `rate` (Desktop's `PlaybackRateButton.nextPlaybackRate`).
pub fn next_rate(rate: f32) -> f32 {
    let at = RATES.iter().position(|r| (r - rate).abs() < 0.01).unwrap_or(0);
    RATES[(at + 1) % RATES.len()]
}

/// "1×", "1.5×": the rate button's label.
pub fn rate_label(rate: f32) -> String {
    let text = format!("{rate}");
    format!("{text}×")
}

/// The loudest sample in each of `bars` equal stretches of `samples`, as a
/// share of the loudest overall (Desktop's peaks, normalised).
pub fn peaks(samples: impl IntoIterator<Item = f32>, total: usize, bars: usize) -> Vec<f32> {
    let mut peaks = vec![0f32; bars];
    if total == 0 || bars == 0 {
        return peaks;
    }
    for (ix, sample) in samples.into_iter().enumerate().take(total) {
        let bar = ix * bars / total;
        peaks[bar] = peaks[bar].max(sample.abs());
    }
    let loudest = peaks.iter().copied().fold(0f32, f32::max);
    if loudest > 0. {
        for peak in &mut peaks {
            *peak /= loudest;
        }
    }
    peaks
}

/// Something playing an audio file: [`Player`], or in tests a silent
/// stand-in (tests never play sound).
pub trait Playback {
    fn play(&self);
    fn pause(&self);
    fn is_playing(&self) -> bool;
    /// Where it is, in seconds.
    fn position(&self) -> f64;
    fn duration(&self) -> f64;
    fn seek(&self, seconds: f64);
    fn set_rate(&self, rate: f32);
}

/// Opens a file to play.
pub type Opener = fn(&Path) -> anyhow::Result<Box<dyn Playback>>;

/// The real player.
pub fn open(path: &Path) -> anyhow::Result<Box<dyn Playback>> {
    Ok(Box::new(Player::open(path)?))
}

impl Playback for Player {
    fn play(&self) {
        Player::play(self)
    }
    fn pause(&self) {
        Player::pause(self)
    }
    fn is_playing(&self) -> bool {
        Player::is_playing(self)
    }
    fn position(&self) -> f64 {
        Player::position(self)
    }
    fn duration(&self) -> f64 {
        Player::duration(self)
    }
    fn seek(&self, seconds: f64) {
        Player::seek(self, seconds)
    }
    fn set_rate(&self, rate: f32) {
        Player::set_rate(self, rate)
    }
}

/// A silent player for tests: it "plays" for a moment of real time, at its
/// last second, then stops and rewinds as `AVAudioPlayer` does at the end.
#[cfg(test)]
pub mod silent {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    pub struct Silent {
        started: Cell<Option<Instant>>,
    }

    pub const LENGTH: f64 = 2.;
    /// How long it plays, in real time.
    const PLAYS_FOR: Duration = Duration::from_millis(300);

    pub fn open(_: &std::path::Path) -> anyhow::Result<Box<dyn super::Playback>> {
        Ok(Box::new(Silent::default()))
    }

    impl super::Playback for Silent {
        fn play(&self) {
            if self.started.get().is_none() {
                self.started.set(Some(Instant::now()));
            }
        }
        fn pause(&self) {
            self.started.set(None);
        }
        fn is_playing(&self) -> bool {
            self.started.get().is_some_and(|at| at.elapsed() < PLAYS_FOR)
        }
        fn position(&self) -> f64 {
            if self.is_playing() { LENGTH - 0.1 } else { 0. }
        }
        fn duration(&self) -> f64 {
            LENGTH
        }
        fn seek(&self, _: f64) {}
        fn set_rate(&self, _: f32) {}
    }
}

/// An audio file being played (or paused); dropping it stops it.
pub struct Player {
    #[cfg(target_os = "macos")]
    player: objc2::rc::Retained<objc2_avf_audio::AVAudioPlayer>,
    #[cfg(target_os = "linux")]
    player: linux::PipePlayer,
}

impl Player {
    /// Opens `path`, ready to play.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            use objc2::AnyThread as _;
            use objc2_avf_audio::AVAudioPlayer;
            let url = file_url(path)?;
            let player = unsafe { AVAudioPlayer::initWithContentsOfURL_error(AVAudioPlayer::alloc(), &url) }
                .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))?;
            unsafe {
                player.setEnableRate(true);
                player.prepareToPlay();
            }
            Ok(Self { player })
        }
        #[cfg(target_os = "linux")]
        {
            let length = linux::duration(path)?;
            Ok(Self { player: linux::PipePlayer::open(path, length)? })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = path;
            anyhow::bail!("audio playback isn't available here")
        }
    }

    pub fn play(&self) {
        #[cfg(target_os = "macos")]
        unsafe {
            self.player.play();
        }
        #[cfg(target_os = "linux")]
        self.player.play();
    }

    pub fn pause(&self) {
        #[cfg(target_os = "macos")]
        unsafe {
            self.player.pause();
        }
        #[cfg(target_os = "linux")]
        self.player.pause();
    }

    pub fn is_playing(&self) -> bool {
        #[cfg(target_os = "macos")]
        return unsafe { self.player.isPlaying() };
        #[cfg(target_os = "linux")]
        return self.player.is_playing();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        false
    }

    /// Where it is, in seconds.
    pub fn position(&self) -> f64 {
        #[cfg(target_os = "macos")]
        return unsafe { self.player.currentTime() };
        #[cfg(target_os = "linux")]
        return self.player.position();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        0.
    }

    pub fn duration(&self) -> f64 {
        #[cfg(target_os = "macos")]
        return unsafe { self.player.duration() };
        #[cfg(target_os = "linux")]
        return self.player.duration();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        0.
    }

    pub fn seek(&self, seconds: f64) {
        #[cfg(target_os = "macos")]
        unsafe {
            self.player.setCurrentTime(seconds.clamp(0., self.duration()));
        }
        #[cfg(target_os = "linux")]
        self.player.seek(seconds);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = seconds;
    }

    pub fn set_rate(&self, rate: f32) {
        #[cfg(target_os = "macos")]
        unsafe {
            self.player.setRate(rate);
        }
        #[cfg(target_os = "linux")]
        self.player.set_rate(rate);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = rate;
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        unsafe {
            self.player.stop();
        }
    }
}

#[cfg(target_os = "macos")]
fn file_url(path: &Path) -> anyhow::Result<objc2::rc::Retained<objc2_foundation::NSURL>> {
    let path = path.to_str().ok_or_else(|| anyhow::anyhow!("not a UTF-8 path"))?;
    Ok(objc2_foundation::NSURL::fileURLWithPath(&objc2_foundation::NSString::from_str(path)))
}

/// Linux's audio: ffmpeg decodes to raw PCM and rodio plays it. ffmpeg is
/// the same dependency the looping videos use (video.rs), and it reads
/// everything Signal sends: AAC/M4A, MP3, Opus. rodio is used without its
/// own decoders, so there's one decode path.
#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use std::io::Read as _;
    use std::num::NonZero;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{sync_channel, Receiver};
    use std::sync::{Arc, Mutex, OnceLock};

    use anyhow::Context as _;

    use crate::{audio, video};

    /// The output device, opened once and kept for the app's lifetime. A
    /// failed open isn't kept, so sound comes back on the next call once
    /// the audio service is up (Vernam can start before it).
    pub(crate) fn device() -> anyhow::Result<&'static rodio::MixerDeviceSink> {
        static DEVICE: OnceLock<&'static rodio::MixerDeviceSink> = OnceLock::new();
        if let Some(device) = DEVICE.get() {
            return Ok(device);
        }
        let mut sink = rodio::DeviceSinkBuilder::open_default_sink().ok().context("no audio output device")?;
        sink.log_on_drop(false);
        // A racing caller's device leaks beside ours; both stay alive, one idle.
        let device = &*Box::leak(Box::new(sink));
        Ok(*DEVICE.get_or_init(|| device))
    }

    /// The device's playback rate and channels, which ffmpeg decodes to so
    /// rodio needs no conversion.
    fn device_format() -> anyhow::Result<(u32, u16)> {
        let config = device()?.config();
        Ok((config.sample_rate().get(), config.channel_count().get()))
    }

    /// A media file's length, in seconds.
    pub(crate) fn duration(path: &Path) -> anyhow::Result<f64> {
        let output = Command::new(video::tool("ffprobe"))
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(path)
            .output()
            .context("running ffprobe (install ffmpeg)")?;
        anyhow::ensure!(output.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        // Containers without a stored duration (recordings) print `N/A`;
        // a 0 length opens the file, and playback measures by EOF.
        Ok(String::from_utf8_lossy(&output.stdout).trim().parse::<f64>().unwrap_or(0.).max(0.))
    }

    /// The whole file as mono f32 samples at `rate`.
    fn decode_mono(path: &Path, rate: u32) -> anyhow::Result<Vec<f32>> {
        // `.output()` drains stdout and stderr together, so a chatty
        // stderr can't fill its pipe and deadlock a long decode.
        let output = Command::new(video::tool("ffmpeg"))
            .arg("-v")
            .arg("error")
            .arg("-i")
            .arg(path)
            .args(["-vn", "-ac", "1", "-ar", &rate.to_string(), "-f", "f32le", "-acodec", "pcm_f32le", "pipe:1"])
            .output()
            .context("running ffmpeg (install ffmpeg)")?;
        anyhow::ensure!(output.status.success(), "ffmpeg failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        Ok(output.stdout.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect())
    }

    /// The waveform: 8 kHz mono is enough for [`audio::BARS`] bars (they
    /// are coarse), and cheap on long files. ponytail: still decodes whole
    /// files into memory, as the macOS one does; an hour at 8 kHz is
    /// 115 MB. Upgrade path: peak per chunk as it streams in.
    pub(crate) fn waveform(path: &Path) -> anyhow::Result<(Vec<f32>, f64)> {
        const RATE: u32 = 8000;
        let samples = decode_mono(path, RATE)?;
        let total = samples.len();
        Ok((audio::peaks(samples, total, audio::BARS), total as f64 / RATE as f64))
    }

    /// The whole file decoded for playback, at a fixed stereo 48 kHz.
    /// rodio's uniform converter adapts it to the device, so sounds decode
    /// without (or before) an output device. Small files, decoded once.
    pub(crate) fn decode_for_playback(path: &Path) -> anyhow::Result<(Vec<f32>, u16, u32)> {
        const STEREO: u16 = 2;
        const RATE: u32 = 48_000;
        let mut child = Command::new(video::tool("ffmpeg"))
            .arg("-v")
            .arg("error")
            .arg("-i")
            .arg(path)
            .args(["-vn", "-ac", &STEREO.to_string(), "-ar", &RATE.to_string(), "-f", "f32le", "-acodec", "pcm_f32le", "pipe:1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("running ffmpeg (install ffmpeg)")?;
        let mut bytes = Vec::new();
        child.stdout.take().unwrap().read_to_end(&mut bytes)?;
        let status = child.wait()?;
        anyhow::ensure!(status.success(), "ffmpeg failed to decode {:?}", path);
        let samples = bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        Ok((samples, STEREO, RATE))
    }

    /// ffmpeg decoding a file to PCM, from `start`. The reader thread hands
    /// chunks over a bounded channel: when the player stops pulling, the
    /// channel fills and the thread and ffmpeg wait.
    struct PcmPipe {
        child: Child,
    }

    /// The stream itself, handed to rodio.
    struct PipeSource {
        frames_rx: Receiver<Arc<[f32]>>,
        chunk: Arc<[f32]>,
        at: usize,
        played: Arc<AtomicU64>,
        eof: Arc<AtomicBool>,
        channels: u16,
        rate: u32,
    }

    impl PcmPipe {
        fn start(path: &Path, start: f64) -> anyhow::Result<(Self, PipeSource)> {
            let (rate, channels) = device_format()?;
            let mut child = Command::new(video::tool("ffmpeg"))
                .arg("-v")
                .arg("error")
                .args(["-ss", &format!("{start}"), "-i"])
                .arg(path)
                .args(["-vn", "-ac", &channels.to_string(), "-ar", &rate.to_string(), "-f", "f32le", "-acodec", "pcm_f32le", "pipe:1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .context("running ffmpeg (install ffmpeg)")?;
            let mut out = child.stdout.take().unwrap();
            let (tx, rx) = sync_channel::<Arc<[f32]>>(8);
            let played = Arc::new(AtomicU64::new(0));
            let eof = Arc::new(AtomicBool::new(false));
            let source = PipeSource { frames_rx: rx, chunk: Arc::from(vec![]), at: 0, played: played.clone(), eof: eof.clone(), channels, rate };
            std::thread::Builder::new()
                .name("audio-decode".into())
                .spawn(move || {
                    let mut bytes = vec![0u8; 64 << 10];
                    // A read can stop mid-sample; carry it into the next.
                    let mut carry = Vec::new();
                    loop {
                        let read = match out.read(&mut bytes) {
                            Ok(0) => break,
                            // A signal interrupts the read, not the stream.
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(_) => break,
                            Ok(read) => read,
                        };
                        carry.extend_from_slice(&bytes[..read]);
                        let whole = carry.len() - carry.len() % 4;
                        let mut samples = Vec::with_capacity(whole / 4);
                        for chunk in carry[..whole].chunks_exact(4) {
                            samples.push(f32::from_le_bytes(chunk.try_into().unwrap()));
                        }
                        carry.drain(..whole);
                        // A closed channel means the source is gone.
                        if tx.send(samples.into()).is_err() {
                            break;
                        }
                    }
                    drop(tx);
                })
                .context("spawning the audio decoder")?;
            Ok((Self { child }, source))
        }
    }

    impl Drop for PcmPipe {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Iterator for PipeSource {
        type Item = f32;

        fn next(&mut self) -> Option<f32> {
            if self.at >= self.chunk.len() {
                match self.frames_rx.try_recv() {
                    Ok(chunk) => {
                        self.chunk = chunk;
                        self.at = 0;
                    }
                    // The decoder hasn't kept up: hand the device silence
                    // now rather than stall its callback (every sound
                    // shares the device) until it does.
                    Err(std::sync::mpsc::TryRecvError::Empty) => return Some(0.),
                    Err(_) => {
                        self.eof.store(true, Ordering::Relaxed);
                        return None;
                    }
                }
            }
            let sample = self.chunk[self.at];
            self.at += 1;
            if self.at % self.channels as usize == 0 {
                self.played.fetch_add(1, Ordering::Relaxed);
            }
            Some(sample)
        }
    }

    impl rodio::Source for PipeSource {
        fn channels(&self) -> NonZero<u16> {
            NonZero::new(self.channels).unwrap()
        }

        fn sample_rate(&self) -> NonZero<u32> {
            NonZero::new(self.rate).unwrap()
        }

        fn current_span_len(&self) -> Option<usize> {
            None
        }

        fn total_duration(&self) -> Option<std::time::Duration> {
            None
        }
    }

    /// A file playing through ffmpeg and the device. One instance plays at
    /// a time ([`audio::Player`]); a video's sound track uses another.
    pub(crate) struct PipePlayer {
        path: PathBuf,
        device: &'static rodio::MixerDeviceSink,
        player: rodio::Player,
        length: f64,
        /// Whether it should be playing (kept across seeks).
        wanted: AtomicBool,
        run: Mutex<Run>,
    }

    struct Run {
        pipe: Option<PcmPipe>,
        played: Arc<AtomicU64>,
        eof: Arc<AtomicBool>,
        /// Where in the file this pipe started, at the device's rate.
        base: f64,
        rate: u32,
    }

    impl PipePlayer {
        /// Opens `path`, paused at the start.
        pub(crate) fn open(path: &Path, length: f64) -> anyhow::Result<Self> {
            let device = device()?;
            let player = rodio::Player::connect_new(device.mixer());
            let this = Self {
                path: path.to_owned(),
                device,
                player,
                length,
                wanted: AtomicBool::new(false),
                run: Mutex::new(Run {
                    pipe: None,
                    played: Arc::new(AtomicU64::new(0)),
                    eof: Arc::new(AtomicBool::new(false)),
                    base: 0.,
                    rate: device.config().sample_rate().get(),
                }),
            };
            this.spawn_at(0.)?;
            this.player.pause();
            Ok(this)
        }

        /// Starts ffmpeg at `seconds`, replacing any current one.
        fn spawn_at(&self, seconds: f64) -> anyhow::Result<()> {
            let mut run = self.run.lock().unwrap_or_else(|e| e.into_inner());
            run.pipe = None; // kills the old ffmpeg
            self.player.clear();
            let (pipe, source) = match PcmPipe::start(&self.path, seconds) {
                Ok((pipe, source)) => (pipe, source),
                Err(error) => {
                    run.eof = Arc::new(AtomicBool::new(true));
                    return Err(error);
                }
            };
            run.played = source.played.clone();
            run.eof = source.eof.clone();
            run.base = seconds;
            run.rate = self.device.config().sample_rate().get();
            run.pipe = Some(pipe);
            self.player.append(source);
            Ok(())
        }

        pub(crate) fn play(&self) {
            self.wanted.store(true, Ordering::Relaxed);
            self.player.play();
        }

        pub(crate) fn pause(&self) {
            self.wanted.store(false, Ordering::Relaxed);
            self.player.pause();
        }

        /// Done playing: either paused, or finished and rewound (as
        /// AVAudioPlayer does at the end).
        fn done(&self) -> bool {
            let run = self.run.lock().unwrap_or_else(|e| e.into_inner());
            run.eof.load(Ordering::Relaxed) && self.player.empty()
        }

        pub(crate) fn is_playing(&self) -> bool {
            !self.player.is_paused() && !self.done()
        }

        pub(crate) fn position(&self) -> f64 {
            let run = self.run.lock().unwrap_or_else(|e| e.into_inner());
            if run.eof.load(Ordering::Relaxed) && self.player.empty() {
                return 0.;
            }
            run.base + run.played.load(Ordering::Relaxed) as f64 / run.rate as f64
        }

        pub(crate) fn duration(&self) -> f64 {
            self.length
        }

        pub(crate) fn seek(&self, seconds: f64) {
            let seconds = seconds.clamp(0., self.length.max(0.));
            if self.spawn_at(seconds).is_err() {
                return;
            }
            if self.wanted.load(Ordering::Relaxed) {
                self.player.play();
            } else {
                self.player.pause();
            }
        }

        /// rodio resamples to change speed, so pitch moves with it, where
        /// AVAudioPlayer keeps pitch. ponytail: respawn ffmpeg with
        /// `atempo` if the pitch difference bothers anyone.
        pub(crate) fn set_rate(&self, rate: f32) {
            self.player.set_speed(rate.max(0.05));
        }
    }

    impl Drop for PipePlayer {
        fn drop(&mut self) {
            self.run.lock().unwrap_or_else(|e| e.into_inner()).pipe = None;
        }
    }

    /// Writes `bytes` to a temporary file with `name`'s extension, runs
    /// `play`, and removes it (`ffmpeg` reads some embedded formats only
    /// from a seekable file).
    pub(crate) fn with_temp_file<T>(name: &str, bytes: &[u8], play: impl FnOnce(&Path) -> T) -> anyhow::Result<T> {
        let dir = std::env::temp_dir();
        use std::io::Write as _;
        // `create_new` refuses a name that already exists (a pre-placed
        // symlink or file), so the bytes only ever land in a file this
        // call made.
        for attempt in 0..100 {
            let path = dir.join(format!("vernam-{name}-{}-{attempt}.m4a", std::process::id()));
            let written = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .and_then(|mut file| file.write_all(bytes));
            match written {
                Ok(()) => {
                    let out = play(&path);
                    let _ = std::fs::remove_file(&path);
                    return Ok(out);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("writing a sound to a temporary file"),
            }
        }
        anyhow::bail!("no free temporary file name for a sound")
    }
}


/// The file's waveform: [`BARS`] peaks from 0 to 1, and its length in
/// seconds. Decoding takes a moment, so this belongs off the main thread.
pub fn waveform(path: &Path) -> anyhow::Result<(Vec<f32>, f64)> {
    #[cfg(target_os = "macos")]
    {
        use objc2::AnyThread as _;
        use objc2_avf_audio::{AVAudioFile, AVAudioPCMBuffer};
        let url = file_url(path)?;
        let file = unsafe { AVAudioFile::initForReading_error(AVAudioFile::alloc(), &url) }
            .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))?;
        let format = unsafe { file.processingFormat() };
        let total = unsafe { file.length() }.max(0) as usize;
        let rate = unsafe { format.sampleRate() };
        const CHUNK: u32 = 1 << 16;
        let buffer = unsafe { AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(AVAudioPCMBuffer::alloc(), &format, CHUNK) }
            .ok_or_else(|| anyhow::anyhow!("no buffer for that format"))?;
        let mut samples = Vec::with_capacity(total);
        // (Reading at the end fails without saying why: stop before it.)
        while (unsafe { file.framePosition() }.max(0) as usize) < total {
            unsafe { file.readIntoBuffer_error(&buffer) }.map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))?;
            let frames = unsafe { buffer.frameLength() } as usize;
            if frames == 0 {
                break;
            }
            // The first channel is enough for the shape (Desktop mixes
            // down; the peaks come out much the same).
            let channels = unsafe { buffer.floatChannelData() };
            if channels.is_null() {
                anyhow::bail!("not floating-point audio");
            }
            let first = unsafe { *channels };
            let data = unsafe { std::slice::from_raw_parts(first.as_ptr(), frames) };
            samples.extend_from_slice(data);
        }
        let total = samples.len();
        Ok((peaks(samples, total, BARS), if rate > 0. { total as f64 / rate } else { 0. }))
    }
    #[cfg(target_os = "linux")]
    {
        linux::waveform(path)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        anyhow::bail!("audio isn't available here")
    }
}

/// "0:07", "1:05", "1:00:00": a time as Desktop shows it beside the
/// waveform and under a video, with hours from an hour on
/// (`ts/util/formatDuration.std.ts`).
pub fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.).round() as u64;
    if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_step_as_desktop_does() {
        assert_eq!(next_rate(1.), 1.5);
        assert_eq!(next_rate(2.), 0.5);
        assert_eq!(next_rate(0.5), 1.);
        assert_eq!((rate_label(1.5), rate_label(1.)), ("1.5×".into(), "1×".into()));
        assert_eq!((clock(7.4), clock(65.)), ("0:07".into(), "1:05".into()));
        assert_eq!((clock(3599.), clock(3600.), clock(3907.)), ("59:59".into(), "1:00:00".into(), "1:05:07".into()));
    }

    #[test]
    fn peaks_are_the_loudest_per_bar_normalised() {
        // Quiet, then loud, then nothing.
        let samples = [0.1, -0.2, 0.8, -0.4, 0., 0.];
        assert_eq!(peaks(samples, 6, 3), vec![0.25, 1., 0.]);
        assert_eq!(peaks([0f32; 4], 4, 2), vec![0., 0.], "silence stays flat");
        assert_eq!(peaks([], 0, 3), vec![0., 0., 0.]);
    }

    /// Decodes a generated WAV (no sound is played).
    #[test]
    fn waveforms_come_from_the_decoded_file() {
        // Linux decodes through ffmpeg, which may not be installed;
        // macOS's AVFoundation is always there.
        #[cfg(target_os = "linux")]
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("skipping: no ffmpeg");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        // A second of 8 kHz mono 16-bit: silent, then a loud half.
        let rate = 8000u32;
        let samples: Vec<i16> = (0..rate).map(|i| if i < rate / 2 { 0 } else { if i % 2 == 0 { 20000 } else { -20000 } }).collect();
        let mut wav = Vec::new();
        let data_len = samples.len() as u32 * 2;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&path, wav).unwrap();
        let (peaks, seconds) = waveform(&path).unwrap();
        assert_eq!(peaks.len(), BARS);
        assert!((seconds - 1.).abs() < 0.01, "{seconds}");
        assert_eq!(peaks[0], 0., "the silent half");
        assert_eq!(peaks[BARS - 1], 1., "the loud half");
    }

    /// A pre-existing file at the temporary name is never touched: the
    /// write only lands in a file this call created.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_sound_never_writes_over_a_file_already_there() {
        let name = "clobber-test";
        let planted = std::env::temp_dir().join(format!("vernam-{name}-{}-0.m4a", std::process::id()));
        std::fs::write(&planted, b"not ours").unwrap();
        let seen = linux::with_temp_file(name, b"sound", |path| path.file_name().unwrap().to_string_lossy().into_owned()).unwrap();
        assert_ne!(
            seen,
            planted.file_name().unwrap().to_string_lossy(),
            "the planted name is skipped"
        );
        assert_eq!(std::fs::read(&planted).unwrap(), b"not ours", "the planted file is untouched");
        let _ = std::fs::remove_file(&planted);
    }
}
