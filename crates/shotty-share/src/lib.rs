//! Uploading captures to artifacts.neuhaus.nrw (raw media uploads, unlisted, unguessable links).
//!
//! The token is the artifacts CLI's: `$ARTIFACTS_TOKEN` or `~/.config/artifacts/token`, which must be
//! a regular file readable only by its owner (same rule as the CLI).

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

pub const DEFAULT_URL: &str = "https://artifacts.neuhaus.nrw";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uploaded {
    pub id: String,
    /// The viewer page (with link previews); what gets shared by default.
    pub url: String,
    /// The file itself, e.g. for embedding in Markdown.
    pub direct: String,
    pub version: u32,
}

#[derive(Deserialize)]
struct Created {
    id: String,
    url: String,
    version: u32,
}

pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
}

pub fn default_token_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config/artifacts/token")
}

fn read_token(path: &Path) -> Result<String> {
    let meta = std::fs::symlink_metadata(path).with_context(|| format!("no artifacts token at {}", path.display()))?;
    if !meta.file_type().is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if meta.permissions().mode() & 0o077 != 0 {
        bail!("{} is readable by others; run chmod 600 on it", path.display());
    }
    let token = std::fs::read_to_string(path)?.trim().to_string();
    if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("the token in {} is malformed", path.display());
    }
    Ok(token)
}

/// Content type for the formats the server accepts as raw media.
pub fn media_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => return None,
    })
}

/// Counts bytes as they are read, reporting the fraction sent.
struct Progress<'a, R> {
    inner: R,
    sent: u64,
    total: u64,
    report: &'a mut dyn FnMut(f32),
}

impl<R: Read> Read for Progress<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.sent += n as u64;
        (self.report)((self.sent as f32 / self.total.max(1) as f32).min(1.));
        Ok(n)
    }
}

fn encode_segment(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Client {
    /// `base` defaults to artifacts.neuhaus.nrw, `token_path` to the CLI's token file.
    pub fn new(base: Option<&str>, token_path: Option<&Path>) -> Result<Self> {
        let token = match std::env::var("ARTIFACTS_TOKEN") {
            Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => read_token(token_path.map_or_else(default_token_path, Path::to_path_buf).as_path())?,
        };
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self { base: base.unwrap_or(DEFAULT_URL).trim_end_matches('/').to_string(), token, agent })
    }

    fn check(response: &mut ureq::http::Response<ureq::Body>) -> Result<String> {
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
                .unwrap_or(body);
            bail!("upload failed ({status}): {message}");
        }
        Ok(body)
    }

    /// Uploads one image or video as a new unlisted draft.
    pub fn upload(&self, path: &Path, description: &str, progress: &mut dyn FnMut(f32)) -> Result<Uploaded> {
        let content_type = media_type(path).with_context(|| format!("{} is not an image or video artifacts accepts", path.display()))?;
        // The server wants an NFC name; macOS hands out NFD for typed umlauts.
        let name: String = path.file_name().context("no file name")?.to_string_lossy().nfc().collect();
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let total = file.metadata()?.len();
        let mut reader = Progress { inner: file, sent: 0, total, report: progress };
        let mut response = self
            .agent
            .post(format!("{}/api/drafts", self.base))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", content_type)
            .header("Content-Length", total.to_string())
            .query("filename", &name)
            .query("description", description)
            .send(ureq::SendBody::from_reader(&mut reader))
            .context("uploading")?;
        let created: Created = serde_json::from_str(&Self::check(&mut response)?).context("unexpected server answer")?;
        let direct = format!("{}/{}/v/{}/{}", self.base, created.id, created.version, encode_segment(&name));
        Ok(Uploaded { id: created.id, url: created.url, direct, version: created.version })
    }

    /// Removes a draft and all its versions; the links stop working right away.
    pub fn delete(&self, id: &str) -> Result<()> {
        let mut response = self
            .agent
            .delete(format!("{}/api/drafts/{}", self.base, encode_segment(id)))
            .header("Authorization", format!("Bearer {}", self.token))
            .call()
            .context("deleting")?;
        Self::check(&mut response).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_and_encoding() {
        assert_eq!(media_type(Path::new("a/Shot 1.PNG")), Some("image/png"));
        assert_eq!(media_type(Path::new("clip.mov")), None);
        assert_eq!(encode_segment("Shotty 2026-10-08 at 13.12.06.mp4"), "Shotty%202026-10-08%20at%2013.12.06.mp4");
    }

    /// Against a local artifacts server: `ARTIFACTS_TEST_URL=http://127.0.0.1:3999 ARTIFACTS_TOKEN=… cargo test -p shotty-share -- --ignored`
    #[test]
    #[ignore]
    fn upload_and_delete_roundtrip() {
        let base = std::env::var("ARTIFACTS_TEST_URL").expect("ARTIFACTS_TEST_URL");
        let client = Client::new(Some(&base), None).unwrap();
        let dir = std::env::temp_dir().join("shotty-share-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Shotty test ü.png");
        // A 1×1 PNG.
        std::fs::write(&file, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\rIDATx\x9cc\xf8\x0f\0\0\x01\x01\0\x05\x18\xd8N\0\0\0\0IEND\xaeB`\x82").unwrap();
        let mut last = 0.;
        let up = client.upload(&file, "test", &mut |p| last = p).unwrap();
        assert_eq!(last, 1.);
        let raw = ureq::get(&up.direct).call().unwrap();
        assert_eq!(raw.headers().get("content-type").unwrap(), "image/png");
        client.delete(&up.id).unwrap();
        let gone = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent().get(&up.url).call().unwrap();
        assert_eq!(gone.status().as_u16(), 404);
    }
}
