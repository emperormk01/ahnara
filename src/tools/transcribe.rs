//! Audio transcription tool using faster-whisper (local Whisper model).
//!
//! Provides `transcribe_audio` which shells out to a Python script running
//! faster-whisper. Also exposes `transcribe_audio_sync` for inline use by
//! channels when audio/voice messages arrive.

use crate::orchestrator::{Tool, ToolResult};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Find the transcribe.py script.
/// Search order:
///   1. AUXLO_TRANSCRIBE_SCRIPT env var
///   2. ~/.ahnara/scripts/transcribe.py
///   3. /usr/local/share/ahnara/scripts/transcribe.py
///   4. /usr/local/share/ahnara/transcribe.py (get.sh deploy path)
///   5. scripts/transcribe.py next to binary
///
/// If none found, auto-downloads from GitHub to ~/.ahnara/scripts/transcribe.py.
fn find_script() -> PathBuf {
    if let Ok(custom) = std::env::var("AUXLO_TRANSCRIBE_SCRIPT") {
        let p = PathBuf::from(custom);
        if p.exists() {
            return p;
        }
    }

    let candidates = [
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root")).join(".ahnara/scripts/transcribe.py"),
        PathBuf::from("/usr/local/share/ahnara/scripts/transcribe.py"),
        PathBuf::from("/usr/local/share/ahnara/transcribe.py"),
    ];

    for p in &candidates {
        if p.exists() {
            return p.clone();
        }
    }

    // Also check relative to binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let rel = dir.join("scripts/transcribe.py");
            if rel.exists() {
                return rel;
            }
        }
    }

    // Auto-download to ~/.ahnara/scripts/transcribe.py
    let deploy_path = candidates[0].clone();
    tracing::info!("transcribe.py not found locally, downloading from GitHub...");
    if let Some(parent) = deploy_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let url = "https://raw.githubusercontent.com/emperormk01/Ahnara/master/scripts/transcribe.py";
    let download = std::process::Command::new("curl")
        .args(["-fsSL", url, "-o", deploy_path.to_str().unwrap_or("")])
        .output();
    match download {
        Ok(o) if o.status.success() => {
            tracing::info!("Downloaded transcribe.py to {}", deploy_path.display());
            // Make executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&deploy_path, std::fs::Permissions::from_mode(0o755));
            }
            deploy_path
        }
        _ => {
            tracing::warn!("Failed to download transcribe.py from GitHub");
            deploy_path // return the path anyway so the error message is clear
        }
    }
}

/// Audio file extensions we can transcribe.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "ogg", "opus", "flac", "m4a", "aac", "wma", "webm", "amr", "3gp",
];

/// Check if a file path looks like an audio file.
pub fn is_audio_file(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|ext| AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Check if a file path is a voice message (Telegram voice notes are .ogg).
pub fn is_voice_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("voice_") || lower.ends_with(".ogg") || lower.ends_with(".opus")
}

/// Transcribe an audio file synchronously (blocking). Used by channels for
/// inline transcription of incoming audio.
///
/// Returns the transcribed text, or an error string.
pub fn transcribe_audio_sync(
    audio_path: &str,
    model_size: &str,
    language: Option<&str>,
) -> Result<TranscriptionResult, String> {
    let script = find_script();
    if !script.exists() {
        return Err(format!(
            "Transcription script not found at {}. Run: curl -fsSL https://raw.githubusercontent.com/emperormk01/Ahnara/master/get.sh | bash",
            script.display()
        ));
    }

    ensure_faster_whisper_installed();

    let output = run_transcription_script(&script, audio_path, model_size, language)?;
    parse_transcription_output(output)
}

fn ensure_faster_whisper_installed() {
    let check = std::process::Command::new("python3")
        .args(["-c", "import faster_whisper"])
        .output();
    if check.is_err() || !check.unwrap().status.success() {
        tracing::info!("faster-whisper not found, installing via pip...");
        let pip_args = ["pip3", "pip"]
            .iter()
            .find(|cmd| {
                std::process::Command::new(cmd.to_string())
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            });
        if let Some(pip) = pip_args {
            let _ = std::process::Command::new(pip.to_string())
                .args(["install", "faster-whisper", "--break-system-packages", "-q"])
                .output();
        }
    }
}

fn run_transcription_script(
    script: &std::path::Path,
    audio_path: &str,
    model_size: &str,
    language: Option<&str>,
) -> Result<std::process::Output, String> {
    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&script).arg(audio_path).arg(model_size);
    if let Some(lang) = language {
        cmd.arg(lang);
    }
    cmd.output().map_err(|e| format!("Failed to run transcription: {e}"))
}

fn parse_transcription_output(output: std::process::Output) -> Result<TranscriptionResult, String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(err_json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(err) = err_json.get("error").and_then(|e| e.as_str()) {
                return Err(err.to_string());
            }
        }
        return Err(format!(
            "Transcription failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            if stderr.is_empty() { stdout.as_ref() } else { stderr.as_ref() }
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_transcription_json(&stdout)
}

/// Parsed transcription result.
#[derive(Debug, Clone)]
pub struct TranscriptionResult {
    pub text: String,
    pub language: String,
    pub duration: f64,
    pub segments: Vec<TranscriptionSegment>,
}

#[derive(Debug, Clone)]
pub struct TranscriptionSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

fn parse_transcription_json(stdout: &str) -> Result<TranscriptionResult, String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse transcription JSON: {e}"))?;

    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        return Err(err.to_string());
    }

    let text = v
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    let language = v
        .get("language")
        .and_then(|l| l.as_str())
        .unwrap_or("unknown")
        .to_string();

    let duration = v
        .get("duration")
        .and_then(|d| d.as_f64())
        .unwrap_or(0.0);

    let segments = v
        .get("segments")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|seg| {
                    Some(TranscriptionSegment {
                        start: seg.get("start")?.as_f64()?,
                        end: seg.get("end")?.as_f64()?,
                        text: seg.get("text")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(TranscriptionResult {
        text,
        language,
        duration,
        segments,
    })
}

/// Format a transcription result for injection into the agent message.
pub fn format_transcription(result: &TranscriptionResult) -> String {
    if result.segments.len() <= 1 || result.duration < 30.0 {
        // Short audio: just the full text
        format!(
            "[Transcription | {} | {:.1}s]\n{}",
            result.language, result.duration, result.text
        )
    } else {
        // Long audio: include timestamps
        let mut out = format!(
            "[Transcription | {} | {:.1}s | {} segments]\n",
            result.language,
            result.duration,
            result.segments.len()
        );
        for seg in &result.segments {
            out.push_str(&format!(
                "[{:.1}s-{:.1}s] {}\n",
                seg.start, seg.end, seg.text
            ));
        }
        out
    }
}

// ── Tool implementation ─────────────────────────────────────────────────

pub struct TranscribeAudioTool;

#[async_trait::async_trait]
impl Tool for TranscribeAudioTool {
    fn name(&self) -> &str {
        "transcribe_audio"
    }

    fn description(&self) -> &str {
        "Transcribe an audio or voice file to text using a local Whisper model. \
         Supports MP3, WAV, OGG, OPUS, FLAC, M4A, AAC, WebM, AMR. \
         Returns the full transcript with timestamps and detected language. \
         Use this when you receive an audio/voice file and need to read its contents, \
         or when the user asks you to transcribe something."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the audio file"
                },
                "model_size": {
                    "type": "string",
                    "enum": ["tiny", "base", "small", "medium", "large-v3"],
                    "description": "Whisper model size. 'base' is fast and good for most cases. \
                                    'small' or 'medium' for better accuracy. 'large-v3' is best but slow.",
                    "default": "base"
                },
                "language": {
                    "type": "string",
                    "description": "ISO language code (e.g. 'en', 'fr', 'es'). \
                                    Leave empty for auto-detection.",
                    "default": ""
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let start = Instant::now();

        let path = args
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();

        if path.is_empty() {
            return Ok(ToolResult {
                tool_name: "transcribe_audio".to_string(),
                success: false,
                output: json!("Missing required parameter: path"),
                error: Some("Missing required parameter: path".to_string()),
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        if !Path::new(&path).exists() {
            return Ok(ToolResult {
                tool_name: "transcribe_audio".to_string(),
                success: false,
                output: json!(format!("File not found: {}", path)),
                error: Some(format!("File not found: {}", path)),
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        if !is_audio_file(&path) {
            return Ok(ToolResult {
                tool_name: "transcribe_audio".to_string(),
                success: false,
                output: json!(format!(
                    "Not an audio file (by extension): {}. Supported: mp3, wav, ogg, opus, flac, m4a, aac, wma, webm, amr, 3gp.",
                    path
                )),
                error: Some(format!("Unsupported file extension for transcription: {}", path)),
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let model_size = args
            .get("model_size")
            .and_then(|m| m.as_str())
            .unwrap_or("base");

        let language = args
            .get("language")
            .and_then(|l| l.as_str())
            .filter(|l| !l.is_empty());

        // Run transcription in a blocking thread since faster-whisper is synchronous
        let path_clone = path.clone();
        let model_clone = model_size.to_string();
        let lang_clone = language.map(|s| s.to_string());

        let result = tokio::task::spawn_blocking(move || {
            transcribe_audio_sync(
                &path_clone,
                &model_clone,
                lang_clone.as_deref(),
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("Transcription task panicked: {e}"))?;

        match result {
            Ok(transcription) => {
                let formatted = format_transcription(&transcription);
                let duration_ms = start.elapsed().as_millis() as u64;
                let is_voice_note = is_voice_file(&path);
                Ok(ToolResult {
                    tool_name: "transcribe_audio".to_string(),
                    success: true,
                    output: json!({
                        "transcript": transcription.text,
                        "language": transcription.language,
                        "duration_seconds": transcription.duration,
                        "segment_count": transcription.segments.len(),
                        "is_voice_note": is_voice_note,
                        "formatted": formatted,
                    }),
                    error: None,
                    duration_ms,
                })
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                Ok(ToolResult {
                    tool_name: "transcribe_audio".to_string(),
                    success: false,
                    output: json!(format!("Transcription failed: {e}")),
                    error: Some(e),
                    duration_ms,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_audio_file() {
        assert!(is_audio_file("/path/to/audio.mp3"));
        assert!(is_audio_file("/path/to/voice.ogg"));
        assert!(is_audio_file("/path/to/recording.wav"));
        assert!(is_audio_file("/path/to/podcast.m4a"));
        assert!(is_audio_file("/path/to/speech.opus"));
        assert!(!is_audio_file("/path/to/image.png"));
        assert!(!is_audio_file("/path/to/video.mp4"));
        assert!(!is_audio_file("/path/to/text.txt"));
    }

    #[test]
    fn test_is_voice_file() {
        assert!(is_voice_file("/tmp/voice_123.ogg"));
        assert!(is_voice_file("/tmp/recording.opus"));
        assert!(!is_voice_file("/tmp/song.mp3"));
    }

    #[test]
    fn test_is_audio_file_case_insensitive() {
        // Extensions must be matched case-insensitively (uppercase + mixed)
        assert!(is_audio_file("AUDIO.MP3"));
        assert!(is_audio_file("Recording.WAV"));
        assert!(is_audio_file("clip.Opus"));
        assert!(is_audio_file("voice.OGG"));
        assert!(is_audio_file("song.FlAc"));
    }

    #[test]
    fn test_is_audio_file_all_supported_extensions() {
        // Cover every entry in AUDIO_EXTENSIONS
        for ext in &["mp3", "wav", "ogg", "opus", "flac", "m4a", "aac", "wma", "webm", "amr", "3gp"] {
            assert!(
                is_audio_file(&format!("/tmp/sample.{ext}")),
                "expected .{ext} to be classified as audio"
            );
        }
    }

    #[test]
    fn test_is_audio_file_unsupported_extensions() {
        for path in &[
            "/tmp/image.png",
            "/tmp/photo.jpg",
            "/tmp/video.mp4",
            "/tmp/text.txt",
            "/tmp/doc.pdf",
            "/tmp/script.py",
            "/tmp/archive.zip",
        ] {
            assert!(!is_audio_file(path), "expected {path} to NOT be audio");
        }
    }

    #[test]
    fn test_is_audio_file_no_extension() {
        assert!(!is_audio_file("/tmp/audio"));
        assert!(!is_audio_file("voice"));
        assert!(!is_audio_file(""));
    }

    #[test]
    fn test_is_audio_file_pathless_filename() {
        assert!(is_audio_file("clip.mp3"));
        assert!(is_audio_file("clip.MP3"));
        assert!(!is_audio_file("clip"));
    }

    #[test]
    fn test_is_audio_file_query_string_and_fragment() {
        // Path::new("/a/b.mp3?x=1").extension() == Some("mp3?x=1") which is
        // not in the list -- this documents the current behavior. We assert it
        // so any future fix is intentional.
        assert!(!is_audio_file("/a/b.mp3?x=1"));
    }

    #[test]
    fn test_is_voice_file_ogg_and_opus_extensions() {
        // Any path ending in .ogg or .opus is treated as a voice message
        assert!(is_voice_file("/tmp/voice_1.ogg"));
        assert!(is_voice_file("/tmp/voice_1.opus"));
        assert!(is_voice_file("song.OGG"));
        assert!(is_voice_file("song.Opus"));
    }

    #[test]
    fn test_is_voice_file_voice_substring() {
        // Telegram download path includes "voice_<id>.ogg" -- the helper
        // should match on the substring pattern, not just the extension.
        assert!(is_voice_file("/var/tmp/voice_42.ogg"));
        assert!(is_voice_file("/home/user/Voice_Notes/clip.ogg"));
        // Other audio extensions are NOT voice by default
        assert!(!is_voice_file("/tmp/song.mp3"));
        assert!(!is_voice_file("/tmp/song.wav"));
        assert!(!is_voice_file("/tmp/recording.m4a"));
    }

    #[test]
    fn test_is_voice_file_empty_and_pathless() {
        assert!(!is_voice_file(""));
        assert!(!is_voice_file("clip"));
        assert!(!is_voice_file("/tmp/audio"));
    }

    #[test]
    fn test_parse_transcription_output() {
        let json = r#"{
            "text": "Hello world, this is a test.",
            "language": "en",
            "language_probability": 0.98,
            "duration": 5.2,
            "segments": [
                {"start": 0.0, "end": 2.5, "text": "Hello world,"},
                {"start": 2.5, "end": 5.2, "text": "this is a test."}
            ]
        }"#;

        let result = parse_transcription_output(json).unwrap();
        assert_eq!(result.text, "Hello world, this is a test.");
        assert_eq!(result.language, "en");
        assert_eq!(result.segments.len(), 2);
        assert!((result.duration - 5.2).abs() < 0.01);
    }

    #[test]
    fn test_parse_error_json() {
        let json = r#"{"error": "File not found: /tmp/bad.mp3"}"#;
        let result = parse_transcription_output(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("File not found"));
    }

    #[test]
    fn test_format_transcription_short() {
        let result = TranscriptionResult {
            text: "Hello world".to_string(),
            language: "en".to_string(),
            duration: 3.0,
            segments: vec![],
        };
        let fmt = format_transcription(&result);
        assert!(fmt.contains("Hello world"));
        assert!(fmt.contains("3.0s"));
    }

    #[test]
    fn test_format_transcription_long() {
        let result = TranscriptionResult {
            text: "Long audio".to_string(),
            language: "en".to_string(),
            duration: 60.0,
            segments: vec![
                TranscriptionSegment { start: 0.0, end: 30.0, text: "First half".to_string() },
                TranscriptionSegment { start: 30.0, end: 60.0, text: "Second half".to_string() },
            ],
        };
        let fmt = format_transcription(&result);
        assert!(fmt.contains("[0.0s-30.0s] First half"));
        assert!(fmt.contains("[30.0s-60.0s] Second half"));
    }
}
