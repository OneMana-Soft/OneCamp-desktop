//! On-device dictation: the microphone, the audio and the speech model all stay
//! on this computer.
//!
//! In a browser, OneCamp's dictation sends the clip to the workspace server,
//! which only works once an administrator has set up a speech engine there. The
//! desktop app can do better: it records natively and transcribes with NVIDIA's
//! Parakeet TDT 0.6B v3, which is fast on an ordinary CPU and handles 25
//! European languages. The approach (cpal to record, transcribe-rs to run the
//! model) follows Handy, the MIT-licensed dictation app.
//!
//! NOTHING IS BUNDLED. The model is about 670 MB and ONNX Runtime adds 8 to 75
//! MB, and most people never dictate, so both are fetched the first time someone
//! asks, from their upstream publishers at pinned versions, and each file is
//! checked against its SHA-256 before it is used. ONNX Runtime is loaded at run
//! time (ort's load-dynamic), so the app starts the same whether it is present
//! or not, and a CPU it cannot run on loses dictation rather than the app.
//!
//! Recording happens natively rather than through the webview's getUserMedia
//! because the webviews differ: WebKitGTK's media support varies by
//! distribution, and a native recorder behaves the same everywhere.

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State};
use transcribe_rs::onnx::{
    parakeet::{ParakeetModel, ParakeetParams},
    Quantization,
};

/// The ONNX Runtime the pinned `ort` (2.0.0-rc.12, API 24) is built against.
const ORT_VERSION: &str = "1.24.4";

/// istupakov's ONNX export of nvidia/parakeet-tdt-0.6b-v3 (CC-BY-4.0), pinned to
/// a commit so the files cannot change under a published hash.
const MODEL_REPO: &str = "istupakov/parakeet-tdt-0.6b-v3-onnx";
const MODEL_REV: &str = "8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce";
const MODEL_DIR: &str = "parakeet-tdt-0.6b-v3-int8";
const MODEL_FILES: &[(&str, u64, &str)] = &[
    ("encoder-model.int8.onnx", 652_183_999, "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09"),
    ("decoder_joint-model.int8.onnx", 18_202_004, "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70"),
    ("nemo128.onnx", 139_764, "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f"),
    ("vocab.txt", 93_939, "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d"),
];

/// Parakeet's input rate.
const MODEL_HZ: u32 = 16_000;
/// A dictation is a message, not a meeting. Recording stops itself here.
const MAX_SECONDS: u64 = 300;
/// The loaded model holds about a gigabyte; it is let go after this long unused.
const IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);

/// One ONNX Runtime build from Microsoft's GitHub release.
#[derive(Debug, PartialEq)]
pub struct Runtime {
    pub archive: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    /// The library's file name inside the archive's `lib/` directory.
    pub lib: &'static str,
}

/// The ONNX Runtime for this computer, or why there is none. Microsoft stopped
/// publishing Intel Mac builds before 1.24.
pub fn runtime() -> Result<Runtime, &'static str> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Ok(Runtime { archive: "onnxruntime-linux-x64-1.24.4.tgz", size: 8_155_822, sha256: "3a211fbea252c1e66290658f1b735b772056149f28321e71c308942cdb54b747", lib: "libonnxruntime.so.1.24.4" });
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Ok(Runtime { archive: "onnxruntime-linux-aarch64-1.24.4.tgz", size: 7_181_958, sha256: "866109a9248d057671a039b9d725be4bd86888e3754140e6701ec621be9d4d7e", lib: "libonnxruntime.so.1.24.4" });
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Ok(Runtime { archive: "onnxruntime-osx-arm64-1.24.4.tgz", size: 30_937_282, sha256: "93787795f47e1eee369182e43ed51b9e5da0878ab0346aecf4258979b8bba989", lib: "libonnxruntime.1.24.4.dylib" });
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Ok(Runtime { archive: "onnxruntime-win-x64-1.24.4.zip", size: 74_442_783, sha256: "d2319fddfb6ea4db99ccc4b60c85c517bcd855721f5daa6a06d40d7cb2ee2357", lib: "onnxruntime.dll" });
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    return Ok(Runtime { archive: "onnxruntime-win-arm64-1.24.4.zip", size: 75_182_070, sha256: "47dc80aa39da792271af10be5993919536a4dab0965ec1e6043ef37f1df7a693", lib: "onnxruntime.dll" });
    #[allow(unreachable_code)]
    Err("On-device dictation needs a Mac with Apple silicon, or Windows or Linux.")
}

/// What the workspace page shows. Tagged so the page can switch on `state`.
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Status {
    Unsupported { reason: String },
    /// Not downloaded yet; `download_mb` is what the first use costs.
    Absent { download_mb: u64 },
    Downloading { done: u64, total: u64 },
    Failed { message: String, download_mb: u64 },
    Ready,
    Recording,
}

struct Recording {
    stop: mpsc::Sender<()>,
    done: thread::JoinHandle<Result<(Vec<f32>, u32), String>>,
}

#[derive(Default)]
pub struct Dictation {
    /// Why the last download failed, until the next one starts.
    failed: Mutex<Option<String>>,
    downloading: Mutex<bool>,
    done: Arc<AtomicU64>,
    recording: Mutex<Option<Recording>>,
    engine: Arc<Mutex<Option<ParakeetModel>>>,
    /// Bumped on each use, so an idle timer can tell it has been superseded.
    uses: Arc<AtomicU64>,
}

fn total_download(rt: &Runtime) -> u64 {
    rt.size + MODEL_FILES.iter().map(|f| f.1).sum::<u64>()
}

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map(|d| d.join("dictation")).map_err(|e| e.to_string())
}

fn runtime_lib(root: &Path, rt: &Runtime) -> PathBuf {
    root.join(format!("onnxruntime-{ORT_VERSION}")).join(rt.lib)
}

/// Every file present at its published size. Hashes are checked when a file is
/// downloaded, not on every status call: hashing 670 MB takes seconds.
pub fn installed(root: &Path, rt: &Runtime) -> bool {
    runtime_lib(root, rt).is_file()
        && MODEL_FILES
            .iter()
            .all(|(name, size, _)| fs::metadata(root.join(MODEL_DIR).join(name)).map(|m| m.len() == *size).unwrap_or(false))
}

fn mb(bytes: u64) -> u64 {
    bytes.div_ceil(1_000_000)
}

#[tauri::command]
pub fn dictation_status(app: AppHandle, state: State<'_, Dictation>) -> Status {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(reason) => return Status::Unsupported { reason: reason.into() },
    };
    if state.recording.lock().unwrap().is_some() {
        return Status::Recording;
    }
    let total = total_download(&rt);
    if *state.downloading.lock().unwrap() {
        return Status::Downloading { done: state.done.load(Ordering::Relaxed), total };
    }
    let Ok(root) = root(&app) else {
        return Status::Unsupported { reason: "OneCamp has no folder to keep the speech model in.".into() };
    };
    if installed(&root, &rt) {
        return Status::Ready;
    }
    match state.failed.lock().unwrap().clone() {
        Some(message) => Status::Failed { message, download_mb: mb(total) },
        None => Status::Absent { download_mb: mb(total) },
    }
}

/// Starts the one-time download in the background and returns at once; the page
/// follows it through `dictation_status`. Calling it again while it runs is a
/// no-op, and files already downloaded are not fetched twice.
#[tauri::command]
pub fn dictation_install(app: AppHandle, state: State<'_, Dictation>) -> Result<(), String> {
    let rt = runtime().map_err(String::from)?;
    {
        let mut busy = state.downloading.lock().unwrap();
        if *busy {
            return Ok(());
        }
        *busy = true;
    }
    *state.failed.lock().unwrap() = None;
    state.done.store(0, Ordering::Relaxed);
    let root = root(&app)?;
    let done = state.done.clone();
    thread::spawn(move || {
        let result = install(&root, &rt, &done);
        let state = app.state::<Dictation>();
        if let Err(e) = result {
            *state.failed.lock().unwrap() = Some(e);
        }
        *state.downloading.lock().unwrap() = false;
    });
    Ok(())
}

fn install(root: &Path, rt: &Runtime, done: &AtomicU64) -> Result<(), String> {
    let model_dir = root.join(MODEL_DIR);
    fs::create_dir_all(&model_dir).map_err(|e| format!("Could not create {}: {e}", model_dir.display()))?;

    let lib = runtime_lib(root, rt);
    if lib.is_file() {
        done.fetch_add(rt.size, Ordering::Relaxed);
    } else {
        let archive = root.join(rt.archive);
        let url = format!("https://github.com/microsoft/onnxruntime/releases/download/v{ORT_VERSION}/{}", rt.archive);
        fetch(&url, &archive, rt.size, rt.sha256, done)?;
        extract_lib(&archive, rt.lib, &lib)?;
        let _ = fs::remove_file(&archive);
    }

    for (name, size, sha) in MODEL_FILES {
        let url = format!("https://huggingface.co/{MODEL_REPO}/resolve/{MODEL_REV}/{name}");
        fetch(&url, &model_dir.join(name), *size, sha, done)?;
    }
    Ok(())
}

/// Downloads `url` to `dest` through a `.part` file, refusing anything that is
/// not exactly the published size and hash. A file already in place at the right
/// size counts as done (its hash was checked when it arrived).
fn fetch(url: &str, dest: &Path, size: u64, sha256: &str, done: &AtomicU64) -> Result<(), String> {
    if fs::metadata(dest).map(|m| m.len() == size).unwrap_or(false) {
        done.fetch_add(size, Ordering::Relaxed);
        return Ok(());
    }
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let part = dest.with_file_name(format!("{name}.part"));
    let offline = |e: &dyn std::fmt::Display| format!("Could not download the speech model ({e}). Check the connection and try again.");

    let response = ureq::get(url).call().map_err(|e| offline(&e))?;
    let mut body = response.into_reader();
    let mut file = fs::File::create(&part).map_err(|e| format!("Could not write {}: {e}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut got = 0u64;
    loop {
        let n = body.read(&mut buf).map_err(|e| offline(&e))?;
        if n == 0 {
            break;
        }
        got += n as u64;
        if got > size {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| format!("Could not write {}: {e}", part.display()))?;
        done.fetch_add(n as u64, Ordering::Relaxed);
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);

    let hash = hex(&hasher.finalize());
    if got != size || hash != sha256 {
        let _ = fs::remove_file(&part);
        done.fetch_sub(got.min(done.load(Ordering::Relaxed)), Ordering::Relaxed);
        return Err(format!("{name} did not match its published checksum, so it was not used. Try again."));
    }
    fs::rename(&part, dest).map_err(|e| e.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Copies `lib/<lib>` out of an ONNX Runtime release archive (a .tgz, or a .zip
/// on Windows) to `dest`, ignoring everything else in it.
fn extract_lib(archive: &Path, lib: &str, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
    let wanted = format!("/lib/{lib}");
    let file = fs::File::open(archive).map_err(|e| e.to_string())?;
    let tmp = dest.with_extension("part");
    let mut found = false;
    if archive.extension().and_then(|e| e.to_str()) == Some("zip") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
            if entry.is_file() && entry.name().ends_with(&wanted) {
                let mut out = fs::File::create(&tmp).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
                found = true;
                break;
            }
        }
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        for entry in tar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path().map_err(|e| e.to_string())?.to_string_lossy().into_owned();
            if entry.header().entry_type().is_file() && path.ends_with(&wanted) {
                let mut out = fs::File::create(&tmp).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
                found = true;
                break;
            }
        }
    }
    if !found {
        return Err(format!("The ONNX Runtime download had no {lib} in it."));
    }
    fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

/// Opens the default microphone and starts recording. Returns once the
/// microphone is actually open, so a refusal (no device, permission denied)
/// reaches the page as an error rather than as an empty transcript later.
#[tauri::command]
pub fn dictation_start(app: AppHandle, state: State<'_, Dictation>) -> Result<(), String> {
    let rt = runtime().map_err(String::from)?;
    if !installed(&root(&app)?, &rt) {
        return Err("Download the speech model first.".into());
    }
    let mut slot = state.recording.lock().unwrap();
    if slot.is_some() {
        return Ok(());
    }
    let (stop_tx, stop_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let done = thread::spawn(move || record(stop_rx, ready_tx));
    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {
            *slot = Some(Recording { stop: stop_tx, done });
            listening(&app, true);
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("The microphone did not start. Check that OneCamp may use it.".into()),
    }
}

/// Runs on its own thread because a cpal stream may not cross threads. Mixes
/// every channel down to mono as it records.
fn record(stop: mpsc::Receiver<()>, ready: mpsc::Sender<Result<(), String>>) -> Result<(Vec<f32>, u32), String> {
    let fail = |ready: &mpsc::Sender<Result<(), String>>, e: String| {
        let _ = ready.send(Err(e.clone()));
        Err(e)
    };
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        return fail(&ready, "No microphone found.".into());
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => return fail(&ready, format!("The microphone could not be opened: {e}")),
    };
    let rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(rate as usize * 30)));
    let cap = rate as usize * MAX_SECONDS as usize;

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => input::<f32>(&device, &config.into(), channels, cap, samples.clone()),
        cpal::SampleFormat::I16 => input::<i16>(&device, &config.into(), channels, cap, samples.clone()),
        cpal::SampleFormat::U16 => input::<u16>(&device, &config.into(), channels, cap, samples.clone()),
        cpal::SampleFormat::I32 => input::<i32>(&device, &config.into(), channels, cap, samples.clone()),
        other => Err(format!("The microphone's sample format ({other}) is not supported.")),
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => return fail(&ready, e),
    };
    if let Err(e) = stream.play() {
        return fail(&ready, format!("The microphone could not start: {e}"));
    }
    let _ = ready.send(Ok(()));
    let _ = stop.recv_timeout(Duration::from_secs(MAX_SECONDS));
    drop(stream);
    let samples = std::mem::take(&mut *samples.lock().unwrap());
    Ok((samples, rate))
}

fn input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    cap: usize,
    samples: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::Sample;
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &_| {
                let mut out = samples.lock().unwrap();
                if out.len() >= cap {
                    return;
                }
                for frame in data.chunks(channels.max(1)) {
                    let sum: f32 = frame.iter().map(|s| f32::from_sample(*s)).sum();
                    out.push(sum / frame.len() as f32);
                }
            },
            |e| eprintln!("microphone error: {e}"),
            None,
        )
        .map_err(|e| format!("The microphone could not be opened: {e}"))
}

/// Stops recording and returns the transcript. The model loads on first use
/// (a few seconds) and stays loaded until it has been idle for ten minutes.
#[tauri::command]
pub async fn dictation_stop(app: AppHandle, state: State<'_, Dictation>) -> Result<String, String> {
    let Some(rec) = state.recording.lock().unwrap().take() else {
        return Err("Nothing is being recorded.".into());
    };
    listening(&app, false);
    let rt = runtime().map_err(String::from)?;
    let root = root(&app)?;
    let engine = state.engine.clone();
    let uses = state.uses.clone();
    let use_id = uses.fetch_add(1, Ordering::SeqCst) + 1;

    let text = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let _ = rec.stop.send(());
        let (samples, rate) = rec.done.join().map_err(|_| "The recording stopped unexpectedly.".to_string())??;
        let audio = resample(&samples, rate, MODEL_HZ);
        // Under a quarter of a second is a mis-click, not speech.
        if audio.len() < MODEL_HZ as usize / 4 {
            return Ok(String::new());
        }
        let mut guard = engine.lock().unwrap();
        if guard.is_none() {
            load_runtime(&runtime_lib(&root, &rt))?;
            let model = ParakeetModel::load(&root.join(MODEL_DIR), &Quantization::Int8)
                .map_err(|e| format!("The speech model could not be loaded: {e}"))?;
            *guard = Some(model);
        }
        let result = guard
            .as_mut()
            .unwrap()
            .transcribe_with(&audio, &ParakeetParams::default())
            .map_err(|e| format!("Transcription failed: {e}"))?;
        Ok(result.text.trim().to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    // Let the model go once nobody has dictated for a while.
    let engine = state.engine.clone();
    thread::spawn(move || {
        thread::sleep(IDLE_UNLOAD);
        if uses.load(Ordering::SeqCst) == use_id {
            if let Ok(mut guard) = engine.try_lock() {
                *guard = None;
            }
        }
    });
    Ok(text)
}

#[tauri::command]
pub fn dictation_cancel(app: AppHandle, state: State<'_, Dictation>) {
    if let Some(rec) = state.recording.lock().unwrap().take() {
        let _ = rec.stop.send(());
        let _ = rec.done.join();
    }
    listening(&app, false);
}

/// The workspace page can open the microphone, so the app says so where the
/// person can see it whatever the page shows: on the tray icon.
fn listening(app: &AppHandle, on: bool) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(if on { "OneCamp is listening…" } else { "OneCamp" }));
    }
}

/// Points `ort` at the downloaded library, once per run.
fn load_runtime(lib: &Path) -> Result<(), String> {
    static LOADED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    LOADED
        .get_or_init(|| {
            ort::init_from(lib)
                .map(|env| {
                    env.with_name("onecamp-dictation").commit();
                })
                .map_err(|e| format!("ONNX Runtime could not be loaded on this computer: {e}"))
        })
        .clone()
}

/// Resamples a whole mono clip. Dictation is transcribed after it ends, so one
/// pass at the end is simpler than resampling as it records, and the result is
/// trimmed to exactly the expected length (the resampler's delay removed).
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    use rubato::{FftFixedIn, Resampler};
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    const CHUNK: usize = 1024;
    let Ok(mut r) = FftFixedIn::<f32>::new(from as usize, to as usize, CHUNK, 2, 1) else {
        return Vec::new();
    };
    let expected = (input.len() as u64 * to as u64 / from as u64) as usize;
    let delay = r.output_delay();
    let mut out = Vec::with_capacity(expected + delay + CHUNK);
    let mut pos = 0;
    while pos + CHUNK <= input.len() {
        match r.process(&[&input[pos..pos + CHUNK]], None) {
            Ok(o) => out.extend_from_slice(&o[0]),
            Err(_) => return Vec::new(),
        }
        pos += CHUNK;
    }
    if pos < input.len() {
        if let Ok(o) = r.process_partial(Some(&[&input[pos..]]), None) {
            out.extend_from_slice(&o[0]);
        }
    }
    let mut rounds = 0;
    while out.len() < expected + delay && rounds < 8 {
        rounds += 1;
        match r.process_partial::<&[f32]>(None, None) {
            Ok(o) if !o[0].is_empty() => out.extend_from_slice(&o[0]),
            _ => break,
        }
    }
    out.drain(..delay.min(out.len()));
    out.truncate(expected);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_keeps_duration_and_pitch() {
        let from = 48_000u32;
        let secs = 1.5f32;
        let n = (from as f32 * secs) as usize;
        let tone: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / from as f32).sin()).collect();
        let out = resample(&tone, from, MODEL_HZ);
        assert_eq!(out.len(), (MODEL_HZ as f32 * secs) as usize);
        // Count rising zero crossings in the steady middle: 440 Hz stays 440 Hz.
        let mid = &out[out.len() / 4..out.len() * 3 / 4];
        let crossings = mid.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() as f32;
        let hz = crossings / (mid.len() as f32 / MODEL_HZ as f32);
        assert!((hz - 440.0).abs() < 5.0, "{hz} Hz");
    }

    #[test]
    fn resample_passes_through_at_the_model_rate() {
        let clip = vec![0.25f32; 1000];
        assert_eq!(resample(&clip, MODEL_HZ, MODEL_HZ), clip);
        assert!(resample(&[], 44_100, MODEL_HZ).is_empty());
    }

    #[test]
    fn installed_needs_every_file_at_its_size() {
        let Ok(rt) = runtime() else { return };
        let dir = std::env::temp_dir().join(format!("onecamp-dictation-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(!installed(&dir, &rt));
        fs::create_dir_all(dir.join(MODEL_DIR)).unwrap();
        fs::create_dir_all(runtime_lib(&dir, &rt).parent().unwrap()).unwrap();
        fs::write(runtime_lib(&dir, &rt), b"lib").unwrap();
        for (name, size, _) in MODEL_FILES {
            let f = fs::File::create(dir.join(MODEL_DIR).join(name)).unwrap();
            f.set_len(*size).unwrap();
        }
        assert!(installed(&dir, &rt));
        fs::File::create(dir.join(MODEL_DIR).join("vocab.txt")).unwrap().set_len(10).unwrap();
        assert!(!installed(&dir, &rt), "a truncated file is not installed");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_serialises_for_the_page() {
        let s = serde_json::to_value(Status::Downloading { done: 5, total: 10 }).unwrap();
        assert_eq!(s, serde_json::json!({ "state": "downloading", "done": 5, "total": 10 }));
        assert_eq!(serde_json::to_value(Status::Ready).unwrap(), serde_json::json!({ "state": "ready" }));
    }

    /// The whole chain on a real clip: the pinned downloads and their checksums,
    /// extracting ONNX Runtime, loading it at run time, resampling and Parakeet.
    /// Downloads ~700 MB, so it runs only on request:
    ///   DICTATION_E2E_WAV=clip.wav DICTATION_E2E_EXPECT="hello" cargo test -- --ignored end_to_end
    /// with a 16-bit PCM mono WAV at any sample rate.
    #[test]
    #[ignore]
    fn end_to_end() {
        let wav = std::env::var("DICTATION_E2E_WAV").expect("DICTATION_E2E_WAV");
        let expect = std::env::var("DICTATION_E2E_EXPECT").unwrap_or_default().to_lowercase();
        let rt = runtime().expect("a supported platform");
        let root = std::env::temp_dir().join("onecamp-dictation-e2e");
        let done = AtomicU64::new(0);
        install(&root, &rt, &done).expect("install");
        assert!(installed(&root, &rt));
        assert_eq!(done.load(Ordering::Relaxed), total_download(&rt));

        let bytes = fs::read(wav).unwrap();
        let (samples, rate) = read_wav(&bytes);
        let audio = resample(&samples, rate, MODEL_HZ);
        load_runtime(&runtime_lib(&root, &rt)).expect("ONNX Runtime loads");
        let mut model = ParakeetModel::load(&root.join(MODEL_DIR), &Quantization::Int8).expect("model loads");
        let text = model.transcribe_with(&audio, &ParakeetParams::default()).unwrap().text.to_lowercase();
        eprintln!("transcript: {text}");
        assert!(text.contains(&expect), "{text:?} lacks {expect:?}");
    }

    /// Just enough WAV for the test: 16-bit PCM, any channel count, mixed to mono.
    fn read_wav(b: &[u8]) -> (Vec<f32>, u32) {
        assert_eq!(&b[0..4], b"RIFF");
        let (mut i, mut rate, mut channels) = (12, 0u32, 1usize);
        while i + 8 <= b.len() {
            let id = &b[i..i + 4];
            let len = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
            let body = &b[i + 8..(i + 8 + len).min(b.len())];
            if id == b"fmt " {
                assert_eq!(u16::from_le_bytes([body[0], body[1]]), 1, "PCM only");
                channels = u16::from_le_bytes([body[2], body[3]]) as usize;
                rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                assert_eq!(u16::from_le_bytes([body[14], body[15]]), 16, "16-bit only");
            } else if id == b"data" {
                let pcm: Vec<f32> = body.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect();
                let mono = pcm.chunks(channels).map(|f| f.iter().sum::<f32>() / f.len() as f32).collect();
                return (mono, rate);
            }
            i += 8 + len + (len & 1);
        }
        panic!("no data chunk");
    }

    #[test]
    fn hashes_are_spelled_like_the_published_ones() {
        // fetch() compares hex() against lowercase SHA-256 strings from GitHub and
        // Hugging Face, so pin its spelling to a known digest.
        assert_eq!(hex(&Sha256::digest(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
