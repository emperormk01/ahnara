//! Vision and document understanding tools.
//!
//! Provides `analyze_image` (sends images to the vision model) and
//! `read_document` (extracts text from PDFs).

use crate::orchestrator::{Tool, ToolResult};
use crate::providers::{gateway_endpoint, gateway_model, ContentPart, ImageUrlPayload, Message};
use base64::Engine;
use serde_json::json;
use std::path::Path;

async fn call_vision_api(messages: serde_json::Value) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(gateway_endpoint())
        .header("Content-Type", "application/json")
        .json(&messages)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Vision API error {}: {}", status, body);
    }

    let v: serde_json::Value = resp.json().await?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("No response from vision model");
    Ok(content.to_string())
}

/// Supported image MIME types, derived from file extension.
fn mime_from_ext(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tiff" | "tif" => "image/tiff",
        _ => "application/octet-stream",
    }
}

/// Read an image file and return (base64_data, mime_type).
fn read_image_base64(path: &Path) -> anyhow::Result<(String, &'static str)> {
    let data = std::fs::read(path)?;
    let mime = mime_from_ext(path);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    Ok((b64, mime))
}

/// Build the data URL for an image.
pub fn read_image_as_data_url(path: &Path) -> anyhow::Result<String> {
    let (b64, mime) = read_image_base64(path)?;
    Ok(format!("data:{};base64,{}", mime, b64))
}

/// Build a multimodal user message with an image.
pub fn build_image_message(
    text: &str,
    image_path: &Path,
    detail: Option<&str>,
) -> anyhow::Result<Message> {
    let data_url = read_image_as_data_url(image_path)?;
    Ok(Message {
        role: "user".into(),
        content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
        content_parts: Some(vec![
            ContentPart::Text { text: text.to_string() },
            ContentPart::ImageUrl {
                image_url: ImageUrlPayload {
                    url: data_url,
                    detail: detail.map(|s| s.to_string()),
                },
            },
        ]),
    })
}

/// Extract text from a PDF using `pdftotext`.
pub fn extract_pdf_text(path: &Path, pages: Option<&str>) -> anyhow::Result<String> {
    let mut cmd = std::process::Command::new("pdftotext");
    if let Some(p) = pages {
        cmd.arg("-f").arg(p.split('-').next().unwrap_or("1"));
        if let Some(end) = p.split('-').nth(1) {
            cmd.arg("-l").arg(end);
        }
    }
    cmd.arg(path).arg("-");
    let output = cmd.output()?;
    if !output.status.success() {
        anyhow::bail!("pdftotext failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Convert PDF pages to PNG images using `pdftoppm`.
pub fn pdf_to_images(path: &Path, out_dir: &Path, pages: Option<&str>) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let prefix = out_dir.join("page");
    let mut cmd = std::process::Command::new("pdftoppm");
    cmd.arg("-png");
    if let Some(p) = pages {
        cmd.arg("-f").arg(p.split('-').next().unwrap_or("1"));
        if let Some(end) = p.split('-').nth(1) {
            cmd.arg("-l").arg(end);
        }
    }
    cmd.arg(path).arg(&prefix);
    let output = cmd.output()?;
    if !output.status.success() {
        anyhow::bail!("pdftoppm failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let parent = prefix.parent().unwrap();
    let stem = prefix.file_name().unwrap().to_string_lossy();
    let mut entries: Vec<_> = std::fs::read_dir(parent)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(&*stem) && name.ends_with(".png")
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());
    Ok(entries.into_iter().map(|e| e.path()).collect())
}

// ─── Tool: analyze_image ────────────────────────────────────────────────────

pub struct AnalyzeImageTool;

#[async_trait::async_trait]
impl Tool for AnalyzeImageTool {
    fn name(&self) -> &str { "analyze_image" }
    fn description(&self) -> &str {
        "Analyze an image file using vision. Send the image to the model with a text prompt. \
         Supports PNG, JPEG, GIF, WebP, BMP, TIFF. The model will see the image and respond \
         to your prompt about it."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the image file"
                },
                "prompt": {
                    "type": "string",
                    "description": "What to ask about the image",
                    "default": "Describe this image in detail"
                },
                "detail": {
                    "type": "string",
                    "enum": ["low", "high", "auto"],
                    "description": "Image detail level (OpenAI-specific: low uses fewer tokens, high for fine detail)",
                    "default": "auto"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str = args["path"].as_str().unwrap_or("");
        let prompt = args["prompt"].as_str().unwrap_or("Describe this image in detail");
        let detail = args["detail"].as_str().unwrap_or("auto");

        let path = Path::new(path_str);
        if !path.exists() {
            return Ok(ToolResult {
                tool_name: "analyze_image".into(),
                success: false,
                output: json!(null),
                error: Some(format!("File not found: {}", path_str)),
                duration_ms: 0,
            });
        }

        let (b64, mime) = read_image_base64(path)?;
        let file_size = std::fs::metadata(path)?.len();
        let data_url = format!("data:{};base64,{}", mime, b64);

        let request = json!({
            "model": gateway_model(),
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": prompt},
                    {"type": "image_url", "image_url": {"url": data_url, "detail": detail}}
                ]
            }],
            "max_tokens": 4096
        });

        let start = std::time::Instant::now();
        match call_vision_api(request).await {
            Ok(response) => Ok(ToolResult {
                tool_name: "analyze_image".into(),
                success: true,
                output: json!({
                    "analysis": response,
                    "file": path_str,
                    "size_bytes": file_size,
                }),
                error: None,
                duration_ms: start.elapsed().as_millis() as u64,
            }),
            Err(e) => Ok(ToolResult {
                tool_name: "analyze_image".into(),
                success: false,
                output: json!(null),
                error: Some(format!("Vision API call failed: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

// ─── Tool: analyze_video ────────────────────────────────────────────────────

pub struct AnalyzeVideoTool;

#[async_trait::async_trait]
impl Tool for AnalyzeVideoTool {
    fn name(&self) -> &str { "analyze_video" }
    fn description(&self) -> &str {
        "Analyze a video file by extracting key frames and sending them to the vision model. \
         Supports MP4, MOV, AVI, MKV, WebM. Extracts evenly-spaced frames using ffmpeg, \
         then sends them as a multi-image prompt to the vision model."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the video file"
                },
                "prompt": {
                    "type": "string",
                    "description": "What to ask about the video",
                    "default": "Describe what happens in this video in detail"
                },
                "max_frames": {
                    "type": "integer",
                    "description": "Maximum number of frames to extract (1-16)",
                    "default": 8
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str = args["path"].as_str().unwrap_or("");
        let prompt = args["prompt"].as_str().unwrap_or("Describe what happens in this video in detail");
        let max_frames = args["max_frames"].as_u64().unwrap_or(8).min(16).max(1) as u32;

        let path = Path::new(path_str);
        if !path.exists() {
            return Ok(ToolResult {
                tool_name: "analyze_video".into(),
                success: false,
                output: json!(null),
                error: Some(format!("File not found: {}", path_str)),
                duration_ms: 0,
            });
        }

        let duration = get_video_duration(path)?;
        if duration <= 0.0 {
            return Ok(ToolResult {
                tool_name: "analyze_video".into(),
                success: false,
                output: json!(null),
                error: Some("Could not determine video duration".to_string()),
                duration_ms: 0,
            });
        }

        let start_t = 0.5_f64;
        let end_t = (duration - 0.5).max(start_t + 0.1);
        let interval = (end_t - start_t) / (max_frames as f64 - 1.0).max(1.0);

        let mut content_parts = vec![serde_json::json!({
            "type": "text",
            "text": format!(
                "This video is {:.1}s long. {} Here are {} key frames extracted at evenly-spaced timestamps. Analyze them in sequence.",
                duration, prompt, max_frames
            )
        })];

        let tmp_dir = std::env::temp_dir().join(format!("auxlo_video_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir)?;

        let mut extracted = 0u32;
        for i in 0..max_frames {
            let timestamp = start_t + (i as f64) * interval;
            let out_path = tmp_dir.join(format!("frame_{:04}.jpg", i));

            let status = std::process::Command::new("ffmpeg")
                .args(["-ss", &format!("{:.2}", timestamp)])
                .args(["-i", path_str])
                .args(["-frames:v", "1"])
                .args(["-q:v", "3"])
                .arg("-y")
                .arg(&out_path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();

            match status {
                Ok(s) if s.success() => {
                    if let Ok(data) = std::fs::read(&out_path) {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                        content_parts.push(json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:image/jpeg;base64,{}", b64),
                                "detail": "auto"
                            }
                        }));
                        extracted += 1;
                    }
                }
                _ => tracing::warn!("ffmpeg failed at {:.2}s", timestamp),
            }
        }

        let _ = std::fs::remove_dir_all(&tmp_dir);

        if extracted == 0 {
            return Ok(ToolResult {
                tool_name: "analyze_video".into(),
                success: false,
                output: json!(null),
                error: Some("Failed to extract any frames. Is ffmpeg installed?".to_string()),
                duration_ms: 0,
            });
        }

        let request = json!({
            "model": gateway_model(),
            "messages": [{"role": "user", "content": content_parts}],
            "max_tokens": 4096
        });

        let file_size = std::fs::metadata(path)?.len();
        let start = std::time::Instant::now();
        match call_vision_api(request).await {
            Ok(response) => Ok(ToolResult {
                tool_name: "analyze_video".into(),
                success: true,
                output: json!({
                    "analysis": response,
                    "file": path_str,
                    "duration_secs": duration,
                    "frames_analyzed": extracted,
                    "size_bytes": file_size,
                }),
                error: None,
                duration_ms: start.elapsed().as_millis() as u64,
            }),
            Err(e) => Ok(ToolResult {
                tool_name: "analyze_video".into(),
                success: false,
                output: json!(null),
                error: Some(format!("Vision API call failed: {}", e)),
                duration_ms: start.elapsed().as_millis() as u64,
            }),
        }
    }
}

/// Get video duration in seconds using ffprobe.
fn get_video_duration(path: &Path) -> anyhow::Result<f64> {
    let output = std::process::Command::new("ffprobe")
        .args(["-v", "error"])
        .args(["-show_entries", "format=duration"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()?;

    if !output.status.success() {
        anyhow::bail!("ffprobe failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let duration = stdout.trim().parse::<f64>()?;
    Ok(duration)
}

// ─── Tool: read_document ────────────────────────────────────────────────────

pub struct ReadDocumentTool;

#[async_trait::async_trait]
impl Tool for ReadDocumentTool {
    fn name(&self) -> &str { "read_document" }
    fn description(&self) -> &str {
        "Extract text content from a PDF document. Returns the full text of the document. \
         For scanned PDFs with no text layer, use analyze_image on individual pages instead."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the PDF file"
                },
                "pages": {
                    "type": "string",
                    "description": "Page range to extract (e.g. '1-5' or '1,3,5'). Omit for all pages.",
                    "default": ""
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str = args["path"].as_str().unwrap_or("");
        let pages_str = args["pages"].as_str().unwrap_or("");
        let pages = if pages_str.is_empty() { None } else { Some(pages_str) };

        let path = Path::new(path_str);
        if !path.exists() {
            return Ok(ToolResult {
                tool_name: "read_document".into(),
                success: false,
                output: json!(null),
                error: Some(format!("File not found: {}", path_str)),
                duration_ms: 0,
            });
        }

        let text = extract_pdf_text(path, pages)?;
        let char_count = text.len();

        if text.trim().is_empty() {
            return Ok(ToolResult {
                tool_name: "read_document".into(),
                success: true,
                output: json!({
                    "text": "",
                    "note": "No extractable text found. This may be a scanned PDF. Use analyze_image on individual pages converted via pdftoppm.",
                    "file": path_str,
                    "chars": 0,
                }),
                error: None,
                duration_ms: 0,
            });
        }

        // Truncate very long documents to avoid context overflow
        let max_chars = 100_000;
        let truncated = if char_count > max_chars {
            format!("{}...\n\n[Truncated: {} total chars, showing first {}]", &text[..max_chars], char_count, max_chars)
        } else {
            text
        };

        Ok(ToolResult {
            tool_name: "read_document".into(),
            success: true,
            output: json!({
                "text": truncated,
                "file": path_str,
                "chars": char_count,
            }),
            error: None,
            duration_ms: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_mime_from_ext() {
        assert_eq!(mime_from_ext(Path::new("test.png")), "image/png");
        assert_eq!(mime_from_ext(Path::new("test.jpg")), "image/jpeg");
        assert_eq!(mime_from_ext(Path::new("test.jpeg")), "image/jpeg");
        assert_eq!(mime_from_ext(Path::new("test.webp")), "image/webp");
        assert_eq!(mime_from_ext(Path::new("test.unknown")), "application/octet-stream");
    }

    #[test]
    fn test_read_image_as_data_url() {
        let dir = std::env::temp_dir().join("auxlo_vision_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.png");
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
            0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41,
            0x54, 0x78, 0x9C, 0x62, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x01, 0xE5, 0x27, 0xDE, 0xFC, 0x00, 0x00,
            0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42,
            0x60, 0x82,
        ];
        std::fs::File::create(&path).unwrap().write_all(png).unwrap();
        let data_url = read_image_as_data_url(&path).unwrap();
        assert!(data_url.starts_with("data:image/png;base64,"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_build_image_message() {
        let dir = std::env::temp_dir().join("auxlo_vision_test2");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.png");
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
            0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41,
            0x54, 0x78, 0x9C, 0x62, 0x00, 0x00, 0x00, 0x02,
            0x00, 0x01, 0xE5, 0x27, 0xDE, 0xFC, 0x00, 0x00,
            0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42,
            0x60, 0x82,
        ];
        std::fs::File::create(&path).unwrap().write_all(png).unwrap();
        let msg = build_image_message("What's in this image?", &path, Some("high")).unwrap();
        assert!(msg.content_parts.is_some());
        let parts = msg.content_parts.unwrap();
        assert_eq!(parts.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_extract_pdf_text_no_file() {
        let result = extract_pdf_text(Path::new("/nonexistent/file.pdf"), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_document_no_file() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tool = ReadDocumentTool;
        let result = rt.block_on(tool.execute(json!({"path": "/nonexistent/file.pdf"}))).unwrap();
        assert!(!result.success);
    }
}
