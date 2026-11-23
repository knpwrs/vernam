//! Videos made smaller before sending, as Signal Desktop does from 8.37
//! (commit 60d50a3: `ts/util/transcodeAttachment.preload.ts`,
//! `ts/util/getTranscodingOptions.preload.ts`): an MP4 is re-encoded as
//! H.264 in an MP4 with its shorter side at most the target resolution and
//! its video bitrate at most the target's (never raised, commit 68662e4),
//! keeping one video and one audio track, written for streaming (Desktop's
//! `fastStart`), without the original's metadata. Desktop sends at its
//! standard quality unless the sender picks high quality for that message;
//! we have no such choice, so it's always standard.
//!
//! AVFoundation does the work, so this is macOS only; elsewhere videos go
//! as they are.

use std::path::Path;

/// One quality step of Desktop's `TranscodingConfigSchema`: for videos up
/// to `max_duration_secs` long.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TranscodeTarget {
    /// The shorter side, in pixels.
    pub resolution: u32,
    pub video_bitrate_mbps: f64,
    pub max_duration_secs: f64,
}

const fn target(resolution: u32, video_bitrate_mbps: f64, max_duration_secs: f64) -> TranscodeTarget {
    TranscodeTarget { resolution, video_bitrate_mbps, max_duration_secs }
}

/// Desktop's `DEFAULT_OPTIONS` (standard quality), by our number's country
/// calling code. Desktop reads `client.attachments.videoTranscodingConfiguration`
/// first, but its config fetch keeps only `desktop.`, `global.` and `cds.`
/// keys (`ts/textsecure/WebAPI.preload.ts`), as ours does, so these are
/// what applies.
const REGIONS: [(&[u16], &[TranscodeTarget]); 2] = [
    (&[], &[target(480, 1.0, 900.)]),
    (
        &[1, 61, 81, 82, 65, 31, 47, 41, 32, 385, 971, 974, 49, 33],
        &[target(720, 2.0, 600.), target(720, 1.5, 900.)],
    ),
];

/// The steps for our number (E.164, `+` and digits): its country's, else
/// the default ones (Desktop's `getRegionConfig`). Country calling codes
/// are prefix-free, so the number's start names its country.
fn region(phone: Option<&str>) -> &'static [TranscodeTarget] {
    let digits = phone.map(|p| p.trim_start_matches('+')).unwrap_or_default();
    REGIONS
        .iter()
        .find(|(codes, _)| !digits.is_empty() && codes.iter().any(|code| digits.starts_with(&code.to_string())))
        .or_else(|| REGIONS.iter().find(|(codes, _)| codes.is_empty()))
        .map_or(&[], |(_, steps)| *steps)
}

/// The step a video of `duration_secs` is encoded with: the first long
/// enough, else the last (Desktop's `getTranscodingOptions`).
pub fn target_for(phone: Option<&str>, duration_secs: f64) -> TranscodeTarget {
    let steps = region(phone);
    steps.iter().find(|s| duration_secs <= s.max_duration_secs).or(steps.last()).copied().unwrap_or(REGIONS[0].1[0])
}

/// The longest video that can be sent, in seconds (Desktop's
/// `getMaxVideoDuration`: the last step's).
pub fn max_duration_secs(phone: Option<&str>) -> f64 {
    target_for(phone, f64::INFINITY).max_duration_secs
}

/// "15 minutes", as Desktop's `VideoFileDuration` toast writes the limit
/// (`Intl.DurationFormat` of whole minutes).
pub fn duration_label(secs: f64) -> String {
    let minutes = (secs / 60.).floor() as u64;
    if minutes == 1 { "1 minute".into() } else { format!("{minutes} minutes") }
}

/// Desktop's MIME types it transcodes (`isVideoTypeTranscodable`): MP4,
/// QuickTime (from commit f820b4a, sent as MP4 once re-encoded), Ogg and
/// WebM. AVFoundation reads MP4 and QuickTime; the others go as they are,
/// as everything does off macOS.
pub fn transcodes(content_type: &str) -> bool {
    cfg!(target_os = "macos") && matches!(content_type, "video/mp4" | "video/quicktime")
}

/// What a video holds, read before it's sent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    pub duration_secs: f64,
    /// The video track's bitrate (bits per second).
    pub video_bps: f64,
    /// The audio track's, or 0 without one.
    pub audio_bps: f64,
    /// Its sound is AAC, copied as it is; any other goes as 128 kbps AAC.
    pub audio_copied: bool,
    /// The size it's shown at, after its rotation.
    pub display_width: u32,
    pub display_height: u32,
}

/// The bitrate sound other than AAC is re-encoded at.
const REENCODED_AUDIO_BPS: f64 = 128_000.;

/// About how big the video comes out at `target`: its tracks' bitrates
/// over its length, the video's at most the target's. Desktop encodes it
/// once to find out (`estimateTranscodedVideoSize`); this is quicker and
/// close enough to decide whether it can be sent.
/// A video whose bitrate isn't known is taken at the target's.
pub fn estimated_size(info: &VideoInfo, target: &TranscodeTarget) -> u64 {
    let wanted = target.video_bitrate_mbps * 1e6;
    let video = if info.video_bps > 0. { info.video_bps.min(wanted) } else { wanted };
    let audio = if info.audio_copied || info.audio_bps == 0. { info.audio_bps } else { REENCODED_AUDIO_BPS };
    ((video + audio) * info.duration_secs / 8.).ceil() as u64
}

/// The size the video is encoded at, before its rotation is applied: its
/// shorter displayed side at most `resolution` (Desktop: a wider video's
/// height, otherwise its width), even numbers as H.264 wants.
/// `natural`: its stored size; `rotated`: displayed a quarter turn round.
pub fn encoded_size(natural: (u32, u32), rotated: bool, resolution: u32) -> (u32, u32) {
    let (w, h) = natural;
    let (display_w, display_h) = if rotated { (h, w) } else { (w, h) };
    let short = if display_w > display_h { display_h } else { display_w };
    let scale = if short > resolution && short > 0 { f64::from(resolution) / f64::from(short) } else { 1. };
    let even = |n: u32| ((f64::from(n) * scale / 2.).round() as u32 * 2).max(2);
    (even(w), even(h))
}

/// Reads a video's length, bitrates and size. `None` when it can't be
/// read as a video.
pub fn probe(path: &Path) -> Option<VideoInfo> {
    #[cfg(target_os = "macos")]
    {
        mac::probe(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// Encodes `input` to `output` (a new file) at `target`. The caller
/// decides whether the result is worth sending.
pub fn transcode(input: &Path, output: &Path, target: &TranscodeTarget) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // On tokio's long-lived blocking threads: autoreleased objects go
        // when it's done.
        objc2::rc::autoreleasepool(|_| mac::transcode(input, output, target))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (input, output, target);
        anyhow::bail!("videos can't be re-encoded here")
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use anyhow::{Context as _, bail};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_av_foundation::{
        AVAsset, AVAssetReader, AVAssetReaderStatus, AVAssetReaderTrackOutput, AVAssetTrack, AVAssetWriter, AVAssetWriterInput,
        AVAssetWriterStatus, AVFileTypeMPEG4, AVMediaTypeAudio, AVMediaTypeVideo, AVURLAsset, AVVideoAverageBitRateKey,
        AVVideoCodecKey, AVVideoCodecTypeH264, AVVideoCompressionPropertiesKey, AVVideoHeightKey, AVVideoWidthKey,
    };
    use objc2_core_media::{CMFormatDescription, CMTime};
    use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

    use super::{TranscodeTarget, VideoInfo};

    /// Core Audio's `kAudioFormatMPEG4AAC` and `kAudioFormatLinearPCM`.
    const AAC: u32 = u32::from_be_bytes(*b"aac ");
    const LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");

    /// Sound settings: 48 kHz stereo in `format`, 16-bit interleaved
    /// little-endian for PCM. AVFoundation's audio keys are strings equal
    /// to their names (`AVFormatIDKey` is @"AVFormatIDKey").
    fn audio_settings(format: u32, extra: &[(&str, i64)]) -> Retained<NSDictionary<NSString, AnyObject>> {
        let mut pairs: Vec<(Retained<NSString>, Retained<NSNumber>)> = vec![
            (NSString::from_str("AVFormatIDKey"), NSNumber::new_u32(format)),
            (NSString::from_str("AVSampleRateKey"), NSNumber::new_f64(48_000.)),
            (NSString::from_str("AVNumberOfChannelsKey"), NSNumber::new_i64(2)),
        ];
        if format == LINEAR_PCM {
            pairs.push((NSString::from_str("AVLinearPCMBitDepthKey"), NSNumber::new_i64(16)));
            for key in ["AVLinearPCMIsFloatKey", "AVLinearPCMIsBigEndianKey", "AVLinearPCMIsNonInterleaved"] {
                pairs.push((NSString::from_str(key), NSNumber::new_bool(false)));
            }
        }
        pairs.extend(extra.iter().map(|(key, value)| (NSString::from_str(key), NSNumber::new_i64(*value))));
        let refs: Vec<(&NSString, &AnyObject)> = pairs.iter().map(|(k, v)| (&**k, v.as_ref() as &AnyObject)).collect();
        dictionary(&refs)
    }

    /// A writer that hasn't finished by then has stalled.
    const STALL: Duration = Duration::from_secs(60);

    fn url(path: &Path) -> anyhow::Result<Retained<NSURL>> {
        let path = path.to_str().context("not a UTF-8 path")?;
        Ok(NSURL::fileURLWithPath(&NSString::from_str(path)))
    }

    fn asset(path: &Path) -> anyhow::Result<Retained<AVURLAsset>> {
        Ok(unsafe { AVURLAsset::URLAssetWithURL_options(&*url(path)?, None) })
    }

    fn first_track(asset: &AVAsset, video: bool) -> Option<Retained<AVAssetTrack>> {
        let kind = unsafe { if video { AVMediaTypeVideo } else { AVMediaTypeAudio } }?;
        #[allow(deprecated)]
        let tracks = unsafe { asset.tracksWithMediaType(kind) };
        tracks.firstObject()
    }

    /// Whether the track is shown a quarter turn round.
    fn rotated(track: &AVAssetTrack) -> bool {
        let t = unsafe { track.preferredTransform() };
        t.a.abs() < 0.5 && t.b.abs() > 0.5
    }

    fn natural_size(track: &AVAssetTrack) -> (u32, u32) {
        let size = unsafe { track.naturalSize() };
        (size.width.abs().round() as u32, size.height.abs().round() as u32)
    }

    fn seconds(time: CMTime) -> f64 {
        let s = unsafe { time.seconds() };
        if s.is_finite() { s } else { 0. }
    }

    /// `None` without a length or a size: it can't be judged as a video.
    pub(super) fn probe(path: &Path) -> Option<VideoInfo> {
        // It runs on long-lived threads: what AVFoundation autoreleases
        // goes when it's done.
        objc2::rc::autoreleasepool(|_| probe_in_pool(path))
    }

    fn probe_in_pool(path: &Path) -> Option<VideoInfo> {
        let asset = asset(path).ok()?;
        let video = first_track(&asset, true)?;
        let audio = first_track(&asset, false);
        let (w, h) = natural_size(&video);
        let (display_width, display_height) = if rotated(&video) { (h, w) } else { (w, h) };
        let duration_secs = seconds(unsafe { asset.duration() });
        if duration_secs <= 0. || w == 0 || h == 0 {
            return None;
        }
        let audio_copied = audio.as_ref().is_some_and(|track| {
            let descriptions = unsafe { track.formatDescriptions() };
            descriptions.firstObject().is_some_and(|d| {
                let d = unsafe { &*(&*d as *const AnyObject).cast::<CMFormatDescription>() };
                (unsafe { d.media_sub_type() }) == AAC
            })
        });
        Some(VideoInfo {
            duration_secs,
            audio_copied,
            video_bps: f64::from(unsafe { video.estimatedDataRate() }),
            audio_bps: audio.map_or(0., |a| f64::from(unsafe { a.estimatedDataRate() })),
            display_width,
            display_height,
        })
    }

    fn dictionary(pairs: &[(&NSString, &AnyObject)]) -> Retained<NSDictionary<NSString, AnyObject>> {
        let (keys, values): (Vec<&NSString>, Vec<&AnyObject>) = pairs.iter().copied().unzip();
        NSDictionary::from_slices(&keys, &values)
    }

    fn cf_key(key: &objc2_core_foundation::CFString) -> &NSString {
        // CFString and NSString are toll-free bridged.
        unsafe { &*(key as *const objc2_core_foundation::CFString).cast::<NSString>() }
    }

    /// An AVFoundation object moved to the thread that drives one track.
    /// Each input and output is used by that thread alone, as Apple's
    /// examples drive each from its own queue.
    struct Sendable<T>(T);
    unsafe impl<T> Send for Sendable<T> {}

    /// Copies one track's samples from reader to writer until it ends;
    /// `false` when the writer refuses one, fails, or stops taking them.
    fn pump(output: &AVAssetReaderTrackOutput, input: &AVAssetWriterInput, writer: &AVAssetWriter) -> bool {
        let mut progress = Instant::now();
        loop {
            if !unsafe { input.isReadyForMoreMediaData() } {
                if progress.elapsed() > STALL || unsafe { writer.status() } == AVAssetWriterStatus::Failed {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            // Each sample's autoreleased objects go with it.
            let step = objc2::rc::autoreleasepool(|_| match unsafe { output.copyNextSampleBuffer() } {
                Some(sample) => Some(unsafe { input.appendSampleBuffer(&sample) }),
                None => None,
            });
            match step {
                Some(true) => progress = Instant::now(),
                Some(false) => return false,
                None => {
                    unsafe { input.markAsFinished() };
                    return true;
                }
            }
        }
    }

    pub(super) fn transcode(input: &Path, output: &Path, target: &TranscodeTarget) -> anyhow::Result<()> {
        let source = asset(input)?;
        let video = first_track(&source, true).context("no video track")?;
        let audio = first_track(&source, false);
        let natural = natural_size(&video);
        if natural.0 == 0 || natural.1 == 0 {
            bail!("the video has no size");
        }
        let (width, height) = super::encoded_size(natural, rotated(&video), target.resolution);
        // Never above the source's (Desktop commit 68662e4), whole bits
        // (commit beb0a9f).
        let source_bps = f64::from(unsafe { video.estimatedDataRate() });
        let wanted = target.video_bitrate_mbps * 1e6;
        let bitrate = if source_bps > 0. { source_bps.min(wanted) } else { wanted }.floor() as i64;

        let reader = unsafe { AVAssetReader::assetReaderWithAsset_error(&source) }
            .map_err(|e| anyhow::anyhow!("can't read the video: {}", e.localizedDescription()))?;
        // Decoded and scaled as the reader goes (it scales 8-bit formats).
        let pixel_format = NSNumber::new_u32(objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        let (w, h) = (NSNumber::new_u32(width), NSNumber::new_u32(height));
        let decoded = unsafe {
            dictionary(&[
                (cf_key(objc2_core_video::kCVPixelBufferPixelFormatTypeKey), &pixel_format),
                (cf_key(objc2_core_video::kCVPixelBufferWidthKey), &w),
                (cf_key(objc2_core_video::kCVPixelBufferHeightKey), &h),
            ])
        };
        let video_out = unsafe { AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(&video, Some(&decoded)) };
        unsafe { video_out.setAlwaysCopiesSampleData(false) };
        // AVFoundation raises (and the process aborts) on an output it
        // can't take, so each is asked first.
        if !unsafe { reader.canAddOutput(&video_out) } {
            bail!("the reader won't decode the video");
        }
        unsafe { reader.addOutput(&video_out) };
        // AAC sound is copied as it is; any other is re-encoded as AAC, as
        // Desktop's conversion does.
        let descriptions = audio.as_ref().map(|track| unsafe { track.formatDescriptions() });
        let first = descriptions.as_ref().and_then(|d| d.firstObject());
        // The array holds CMFormatDescriptions, kept alive by `first`.
        let hint = first.as_deref().map(|d| unsafe { &*(d as *const AnyObject).cast::<CMFormatDescription>() });
        let copy_audio = hint.is_some_and(|h| unsafe { h.media_sub_type() } == AAC);
        let pcm = audio_settings(LINEAR_PCM, &[]);
        let audio_out = match &audio {
            Some(track) => {
                let settings = (!copy_audio).then_some(&*pcm);
                let out = unsafe { AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(track, settings) };
                unsafe { out.setAlwaysCopiesSampleData(false) };
                if !unsafe { reader.canAddOutput(&out) } {
                    bail!("the reader won't decode the sound");
                }
                unsafe { reader.addOutput(&out) };
                Some(out)
            }
            None => None,
        };

        let file_type = unsafe { AVFileTypeMPEG4 }.context("no MP4 file type")?;
        let writer = unsafe { AVAssetWriter::assetWriterWithURL_fileType_error(&*url(output)?, file_type) }
            .map_err(|e| anyhow::anyhow!("can't write the video: {}", e.localizedDescription()))?;
        // The index first, so it plays while it downloads.
        unsafe { writer.setShouldOptimizeForNetworkUse(true) };
        let codec = unsafe { AVVideoCodecTypeH264 }.context("no H.264")?;
        let bitrate = NSNumber::new_i64(bitrate);
        let compression = unsafe { dictionary(&[(AVVideoAverageBitRateKey.context("no bitrate key")?, &bitrate)]) };
        let encoded = unsafe {
            dictionary(&[
                (AVVideoCodecKey.context("no codec key")?, codec),
                (AVVideoWidthKey.context("no width key")?, &w),
                (AVVideoHeightKey.context("no height key")?, &h),
                (AVVideoCompressionPropertiesKey.context("no compression key")?, &compression),
            ])
        };
        let video_kind = unsafe { AVMediaTypeVideo }.context("no video type")?;
        let video_in = unsafe { AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(video_kind, Some(&encoded)) };
        // Its rotation, with the move that goes with it in the new size's
        // pixels.
        let mut transform = unsafe { video.preferredTransform() };
        let scale = (f64::from(width) / f64::from(natural.0)) as objc2_core_foundation::CGFloat;
        transform.tx *= scale;
        transform.ty *= scale;
        unsafe { video_in.setTransform(transform) };
        if !unsafe { writer.canAddInput(&video_in) } {
            bail!("the writer won't take the video");
        }
        unsafe { writer.addInput(&video_in) };
        let aac = audio_settings(AAC, &[("AVEncoderBitRateKey", 128_000)]);
        let audio_in = match &audio {
            Some(_) => {
                let kind = unsafe { AVMediaTypeAudio }.context("no audio type")?;
                let input = if copy_audio {
                    unsafe { AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings_sourceFormatHint(kind, None, hint) }
                } else {
                    unsafe { AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(kind, Some(&aac)) }
                };
                if !unsafe { writer.canAddInput(&input) } {
                    bail!("the writer won't take the audio");
                }
                unsafe { writer.addInput(&input) };
                Some(input)
            }
            None => None,
        };

        if !unsafe { reader.startReading() } {
            bail!("reading failed: {:?}", unsafe { reader.error() }.map(|e| e.localizedDescription()));
        }
        if !unsafe { writer.startWriting() } {
            unsafe { reader.cancelReading() };
            bail!("writing failed: {:?}", unsafe { writer.error() }.map(|e| e.localizedDescription()));
        }
        unsafe { writer.startSessionAtSourceTime(objc2_core_media::kCMTimeZero) };

        // Each track on its own thread: the writer interleaves them and
        // waits on whichever is behind.
        let video_job = Sendable((video_out, video_in, writer.clone()));
        let video_thread = std::thread::spawn(move || {
            let job = video_job;
            pump(&job.0.0, &job.0.1, &job.0.2)
        });
        let audio_ok = match (audio_out, audio_in) {
            (Some(out), Some(input)) => pump(&out, &input, &writer),
            _ => true,
        };
        if !audio_ok {
            // So the video's thread isn't left waiting on the reader.
            unsafe { reader.cancelReading() };
        }
        let video_ok = video_thread.join().unwrap_or(false);
        if !video_ok || !audio_ok || unsafe { reader.status() } == AVAssetReaderStatus::Failed {
            unsafe { reader.cancelReading() };
            unsafe { writer.cancelWriting() };
            bail!(
                "re-encoding failed: {:?} {:?}",
                unsafe { reader.error() }.map(|e| e.localizedDescription()),
                unsafe { writer.error() }.map(|e| e.localizedDescription())
            );
        }

        let (done, finished) = mpsc::channel();
        let handler = block2::RcBlock::new(move || {
            let _ = done.send(());
        });
        unsafe { writer.finishWritingWithCompletionHandler(&handler) };
        let started = Instant::now();
        while finished.recv_timeout(Duration::from_millis(200)).is_err() {
            // (Cancelling isn't allowed once finishing has begun.)
            if started.elapsed() > STALL {
                bail!("finishing the video stalled");
            }
        }
        match unsafe { writer.status() } {
            AVAssetWriterStatus::Completed => Ok(()),
            _ => bail!("finishing the video failed: {:?}", unsafe { writer.error() }.map(|e| e.localizedDescription())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_follow_desktops_defaults_by_country() {
        // Elsewhere: 480p at 1 Mbps, up to 15 minutes.
        assert_eq!(target_for(Some("+447700900123"), 60.), target(480, 1.0, 900.));
        assert_eq!(target_for(None, 60.), target(480, 1.0, 900.));
        // The US and others: 720p, 2 Mbps up to 10 minutes, then 1.5.
        assert_eq!(target_for(Some("+15555550123"), 600.), target(720, 2.0, 600.));
        assert_eq!(target_for(Some("+15555550123"), 601.), target(720, 1.5, 900.));
        assert_eq!(target_for(Some("+38591234567"), 60.).resolution, 720);
        // Longer than every step: the last.
        assert_eq!(target_for(Some("+4915123456789"), 5_000.), target(720, 1.5, 900.));
        assert_eq!(max_duration_secs(Some("+15555550123")), 900.);
        assert_eq!(duration_label(900.), "15 minutes");
    }

    #[test]
    fn the_shorter_side_is_scaled_down_never_up() {
        // Landscape 1080p to 480p: height 480.
        assert_eq!(encoded_size((1920, 1080), false, 480), (854, 480));
        // Portrait, stored landscape and turned: the shown width is 480.
        assert_eq!(encoded_size((1920, 1080), true, 480), (854, 480));
        assert_eq!(encoded_size((1080, 1920), false, 720), (720, 1280));
        // Already small enough.
        assert_eq!(encoded_size((640, 360), false, 720), (640, 360));
    }

    /// A real re-encode, of a clip ffmpeg makes (skipped without it).
    #[test]
    fn a_1080p_clip_comes_out_480p_with_its_sound() {
        let dir = tempfile::tempdir().unwrap();
        let (input, output) = (dir.path().join("in.mp4"), dir.path().join("out.mp4"));
        let made = std::process::Command::new("/opt/homebrew/bin/ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=size=1920x1080:rate=30:duration=3"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=3", "-c:v", "libx264", "-b:v", "8M"])
            .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&input)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        let before = probe(&input).expect("the clip reads");
        assert_eq!((before.display_width, before.display_height), (1920, 1080));
        transcode(&input, &output, &target(480, 1.0, 900.)).unwrap();
        let after = probe(&output).expect("the result reads");
        assert_eq!((after.display_width, after.display_height), (854, 480));
        assert!((after.duration_secs - before.duration_secs).abs() < 0.2, "{after:?}");

        // Sound an MP4 can't take as it is (ALAC here) is re-encoded as AAC.
        let (alac, alac_out) = (dir.path().join("alac.mp4"), dir.path().join("alac-out.mp4"));
        let made = std::process::Command::new("/opt/homebrew/bin/ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&input)
            .args(["-c:v", "copy", "-c:a", "alac"])
            .arg(&alac)
            .status();
        assert!(made.is_ok_and(|s| s.success()));
        transcode(&alac, &alac_out, &target(480, 1.0, 900.)).unwrap();
        assert!(probe(&alac_out).unwrap().audio_bps > 0., "the sound is kept");
        assert!(after.audio_bps > 0., "the sound is kept");
        let (size_in, size_out) = (std::fs::metadata(&input).unwrap().len(), std::fs::metadata(&output).unwrap().len());
        assert!(size_out < size_in / 2, "{size_in} -> {size_out}");

        // Stored landscape and shown upright, as phones record: still shown
        // upright, its shown width 480.
        let (turned, turned_out) = (dir.path().join("turned.mp4"), dir.path().join("turned-out.mp4"));
        let made = std::process::Command::new("/opt/homebrew/bin/ffmpeg")
            .args(["-v", "error", "-display_rotation", "90", "-i"])
            .arg(&input)
            .args(["-c", "copy"])
            .arg(&turned)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("this ffmpeg can't set a rotation; skipping that part");
            return;
        }
        let before = probe(&turned).unwrap();
        assert_eq!((before.display_width, before.display_height), (1080, 1920));
        transcode(&turned, &turned_out, &target(480, 1.0, 900.)).unwrap();
        let after = probe(&turned_out).unwrap();
        assert_eq!((after.display_width, after.display_height), (480, 854));
    }

    /// A QuickTime movie re-encodes as an MP4 does (Desktop commit f820b4a).
    #[test]
    fn a_quicktime_movie_re_encodes() {
        assert_eq!(transcodes("video/quicktime"), cfg!(target_os = "macos"));
        assert!(!transcodes("video/webm"));
        let dir = tempfile::tempdir().unwrap();
        let (input, output) = (dir.path().join("in.mov"), dir.path().join("out.mp4"));
        let made = std::process::Command::new("/opt/homebrew/bin/ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=size=1280x720:rate=30:duration=2"])
            .args(["-c:v", "libx264", "-b:v", "6M", "-pix_fmt", "yuv420p", "-f", "mov"])
            .arg(&input)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        transcode(&input, &output, &target(480, 1.0, 900.)).unwrap();
        let after = probe(&output).expect("the result reads");
        assert_eq!((after.display_width, after.display_height), (854, 480));
    }

    #[test]
    fn the_estimate_caps_the_video_bitrate() {
        let info = VideoInfo {
            duration_secs: 80.,
            video_bps: 8e6,
            audio_bps: 128e3,
            audio_copied: true,
            display_width: 1920,
            display_height: 1080,
        };
        // 1 Mbps of video and 128 kbps of audio for 80 s.
        assert_eq!(estimated_size(&info, &target(480, 1.0, 900.)), 11_280_000);
        let low = VideoInfo { video_bps: 5e5, ..info };
        assert_eq!(estimated_size(&low, &target(480, 1.0, 900.)), 6_280_000);
        // Unknown: the target's.
        let unknown = VideoInfo { video_bps: 0., ..info };
        assert_eq!(estimated_size(&unknown, &target(480, 1.0, 900.)), 11_280_000);
        // PCM sound at 1.5 Mbps goes as 128 kbps AAC.
        let pcm = VideoInfo { audio_bps: 1_536e3, audio_copied: false, ..info };
        assert_eq!(estimated_size(&pcm, &target(480, 1.0, 900.)), 11_280_000);
    }
}
