//! Backing logic for the `nomad://` custom URI scheme (registered in the
//! sibling `src-tauri` crate, the only crate that touches `tauri::Builder`).
//! URL shape: `nomad://<identity-hash-hex>/page/<name>` or `/file/<name>`.

use std::time::Duration;

use ratspeak_nomad::FetchedContent;
use ratspeak_runtime::state::AppState;

const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

pub struct NomadResponse {
    pub content_type: &'static str,
    pub body: Vec<u8>,
    pub attachment_name: Option<String>,
}

pub async fn fetch_for_uri(
    state: &AppState,
    identity_hash_hex: &str,
    path: &str,
) -> Result<NomadResponse, String> {
    let identity_hash = parse_identity_hash(identity_hash_hex)?;

    let handle = state
        .rns
        .read()
        .map_err(|_| "network state unavailable".to_string())?
        .as_ref()
        .map(|mgr| mgr.handle.clone())
        .ok_or_else(|| "Reticulum is not running".to_string())?;
    let identity = state
        .lxmf
        .lock()
        .map_err(|_| "identity state unavailable".to_string())?
        .as_ref()
        .map(|mgr| mgr.identity.clone())
        .ok_or_else(|| "no identity is unlocked".to_string())?;

    let content = ratspeak_nomad::fetch(&handle, &identity, identity_hash, path, FETCH_TIMEOUT)
        .await
        .map_err(|e| e.to_string())?;

    Ok(match content {
        FetchedContent::Html(bytes) => NomadResponse {
            content_type: "text/html; charset=utf-8",
            body: bytes,
            attachment_name: None,
        },
        FetchedContent::Micron(bytes) => NomadResponse {
            content_type: "text/html; charset=utf-8",
            body: wrap_html(&ratspeak_nomad::micron_to_html(&bytes)),
            attachment_name: None,
        },
        FetchedContent::File { name, bytes } => NomadResponse {
            content_type: "application/octet-stream",
            body: bytes,
            attachment_name: Some(name),
        },
    })
}

fn parse_identity_hash(hex_str: &str) -> Result<[u8; 16], String> {
    let bytes = hex::decode(hex_str).map_err(|_| "invalid identity hash".to_string())?;
    bytes
        .try_into()
        .map_err(|_| "identity hash must be 16 bytes".to_string())
}

fn wrap_html(fragment: &str) -> Vec<u8> {
    format!("<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>{fragment}</body></html>")
        .into_bytes()
}
