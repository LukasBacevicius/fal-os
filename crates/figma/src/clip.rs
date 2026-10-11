//! Figma's clipboard payload.
//!
//! Copying in Figma puts a `text/html` flavor on the clipboard that carries two HTML comments
//! inside `data-*` attributes:
//!
//! ```html
//! <span data-metadata="<!--(figmeta)BASE64(/figmeta)-->"></span>
//! <span data-buffer="<!--(figma)BASE64(/figma)-->"></span>
//! ```
//!
//! - `figmeta`: JSON `{fileKey, pasteID, dataType, selectedNodeData, imageHashes}` where
//!   `imageHashes` maps the SHA-1 of every image used by the selection to a signed,
//!   unauthenticated download URL (valid for about a week).
//! - `figma`: a fig archive: an 8-byte magic (`fig-kiwi`, `fig-jam.`, …), a `u32` version,
//!   then length-prefixed (`u32` LE) chunks. Chunk 0 is the kiwi schema, chunk 1 the scene
//!   message; each is raw deflate, zlib or zstd compressed (sniffed).

use base64::Engine;
use serde_json::Value as Json;

use crate::kiwi::{Error, Result, Schema, Value};

/// The decoded clipboard: metadata + the scene message.
pub struct Clip {
    pub meta: Json,
    pub version: u32,
    pub message: Value,
}

/// Whether an HTML clipboard payload carries a Figma scene.
pub fn is_figma_html(html: &str) -> bool {
    html.contains("(figma)") && html.contains("(figmeta)")
}

fn between<'a>(s: &'a str, start: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    let rest = &s[i..];
    // Current payloads close with `(/figmeta)-->`, older ones with just `-->`.
    let end = [rest.find("(/"), rest.find("-->"), rest.find('"')].into_iter().flatten().min()?;
    Some(rest[..end].trim())
}

fn b64(s: &str) -> Result<Vec<u8>> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(clean.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(clean.trim_end_matches('=')))
        .map_err(|e| Error(format!("bad base64: {e}")))
}

/// Splits the HTML payload into the metadata JSON and the fig archive bytes.
pub fn extract(html: &str) -> Result<(Json, Vec<u8>)> {
    let meta = between(html, "(figmeta)").ok_or_else(|| Error("no figmeta in clipboard html".into()))?;
    let buf = between(html, "(figma)").ok_or_else(|| Error("no figma buffer in clipboard html".into()))?;
    let meta: Json = serde_json::from_slice(&b64(meta)?).map_err(|e| Error(format!("bad figmeta json: {e}")))?;
    Ok((meta, b64(buf)?))
}

/// The archive's chunks (still compressed) and its version.
pub fn chunks(archive: &[u8]) -> Result<(u32, Vec<&[u8]>)> {
    if archive.len() < 12 || !archive[..8].iter().all(|b| b.is_ascii_graphic()) {
        return Err(Error("not a fig archive".into()));
    }
    let version = u32::from_le_bytes(archive[8..12].try_into().unwrap());
    let mut out = Vec::new();
    let mut i = 12;
    while i + 4 <= archive.len() {
        let n = u32::from_le_bytes(archive[i..i + 4].try_into().unwrap()) as usize;
        i += 4;
        let end = i.checked_add(n).filter(|e| *e <= archive.len()).ok_or_else(|| Error("truncated fig archive chunk".into()))?;
        out.push(&archive[i..end]);
        i = end;
    }
    Ok((version, out))
}

/// Inflates a chunk: zstd (magic `28 b5 2f fd`), zlib (`78 xx`), or raw deflate.
pub fn decompress(chunk: &[u8]) -> Result<Vec<u8>> {
    if chunk.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        use std::io::Read;
        let mut src = chunk;
        let mut dec = ruzstd::decoding::StreamingDecoder::new(&mut src).map_err(|e| Error(format!("zstd: {e}")))?;
        let mut out = Vec::new();
        dec.read_to_end(&mut out).map_err(|e| Error(format!("zstd: {e}")))?;
        return Ok(out);
    }
    if chunk.len() > 2 && chunk[0] == 0x78 && u16::from_be_bytes([chunk[0], chunk[1]]).is_multiple_of(31)
        && let Ok(v) = miniz_oxide::inflate::decompress_to_vec_zlib(chunk)
    {
        return Ok(v);
    }
    miniz_oxide::inflate::decompress_to_vec(chunk).map_err(|e| Error(format!("inflate: {e:?}")))
}

/// Decodes a fig archive (schema chunk + message chunk) into the scene message.
pub fn decode_archive(archive: &[u8]) -> Result<(u32, Value)> {
    let (version, parts) = chunks(archive)?;
    if parts.len() < 2 {
        return Err(Error(format!("fig archive has {} chunks, need 2", parts.len())));
    }
    let schema = Schema::decode(&decompress(parts[0])?)?;
    let message = schema.decode_message("Message", &decompress(parts[1])?)?;
    Ok((version, message))
}

/// Decodes the clipboard HTML.
pub fn decode_html(html: &str) -> Result<Clip> {
    let (meta, archive) = extract(html)?;
    let (version, message) = decode_archive(&archive)?;
    Ok(Clip { meta, version, message })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_payloads() {
        let meta = base64::engine::general_purpose::STANDARD.encode(br#"{"fileKey":"k","imageHashes":{}}"#);
        let buf = base64::engine::general_purpose::STANDARD.encode(b"fig-kiwi\x01\x00\x00\x00");
        let html = format!(r#"<meta charset="utf-8"><span data-metadata="<!--(figmeta){meta}(/figmeta)-->"></span><span data-buffer="<!--(figma){buf}(/figma)-->"></span>"#);
        assert!(is_figma_html(&html));
        let (m, a) = extract(&html).unwrap();
        assert_eq!(m["fileKey"], "k");
        let (v, parts) = chunks(&a).unwrap();
        assert_eq!((v, parts.len()), (1, 0));
    }

    #[test]
    fn inflates_raw_deflate_and_zlib() {
        let data = b"hello hello hello hello".repeat(10);
        let raw = miniz_oxide::deflate::compress_to_vec(&data, 6);
        let zlib = miniz_oxide::deflate::compress_to_vec_zlib(&data, 6);
        assert_eq!(decompress(&raw).unwrap(), data);
        assert_eq!(decompress(&zlib).unwrap(), data);
    }
}
