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

/// Re-exported so `src-tauri` (which doesn't otherwise depend on
/// `ratspeak-nomad` directly) can encode a request's query string into a
/// fetch payload without adding that dependency edge just for this one call.
pub use ratspeak_nomad::encode_form_payload;

/// `payload` is the request data — a Micron form submission's fields,
/// msgpack-encoded (see `ratspeak_nomad::encode_form_payload`), or empty for
/// an ordinary navigation.
pub async fn fetch_for_uri(
    state: &AppState,
    identity_hash_hex: &str,
    path: &str,
    payload: Vec<u8>,
) -> Result<NomadResponse, String> {
    let (handle, identity, identity_hash) = resolve_fetch_args(state, identity_hash_hex)?;
    let content =
        ratspeak_nomad::fetch(&handle, &identity, identity_hash, path, payload, FETCH_TIMEOUT)
            .await
            .map_err(|e| e.to_string())?;
    Ok(to_response(content))
}

/// Same as [`fetch_for_uri`], but sends `(bytes_received, total_bytes)` on
/// `progress` as the file's resource segments arrive — see
/// `ratspeak_nomad::fetch_with_progress`.
pub async fn fetch_for_uri_with_progress(
    state: &AppState,
    identity_hash_hex: &str,
    path: &str,
    payload: Vec<u8>,
    progress: tokio::sync::mpsc::UnboundedSender<(usize, usize)>,
) -> Result<NomadResponse, String> {
    let (handle, identity, identity_hash) = resolve_fetch_args(state, identity_hash_hex)?;
    let content = ratspeak_nomad::fetch_with_progress(
        &handle,
        &identity,
        identity_hash,
        path,
        payload,
        FETCH_TIMEOUT,
        progress,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(to_response(content))
}

fn resolve_fetch_args(
    state: &AppState,
    identity_hash_hex: &str,
) -> Result<
    (
        rns_runtime::reticulum::ReticulumHandle,
        rns_identity::identity::Identity,
        [u8; 16],
    ),
    String,
> {
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

    Ok((handle, identity, identity_hash))
}

fn to_response(content: FetchedContent) -> NomadResponse {
    match content {
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
    }
}

fn parse_identity_hash(hex_str: &str) -> Result<[u8; 16], String> {
    let bytes = hex::decode(hex_str).map_err(|_| "invalid identity hash".to_string())?;
    bytes
        .try_into()
        .map_err(|_| "identity hash must be 16 bytes".to_string())
}

/// Page background for Micron output. Forced, not a fallback: Micron pages
/// always render dark regardless of what the source asked for.
const MICRON_PAGE_BG: &str = "#18171a";
/// Default text colour, chosen to read on `MICRON_PAGE_BG`. Only a default —
/// a page's own `#!fg=` and inline colour runs are inline styles and win.
const MICRON_PAGE_FG: &str = "#f2eeea";

/// Wraps rendered Micron in a document with the forced dark page background.
///
/// Only Micron goes through here; native HTML is passed through untouched and
/// keeps rendering as authored.
///
/// The background is set with `!important` so it also beats the inline style
/// on the page wrapper, which is what a `#!bg=` would otherwise reach. Text
/// colour deliberately is not: it is a plain declaration, so `#!fg=` and the
/// per-run colour spans still override it and keep pages readable.
fn wrap_html(fragment: &str) -> Vec<u8> {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <meta name=\"color-scheme\" content=\"dark\">\
         <style>\
         html,body{{background:{MICRON_PAGE_BG}!important;margin:0;padding:12px}}\
         body{{color:{MICRON_PAGE_FG};{MICRON_GRID_CSS}}}\
         p,h1,h2,h3,h4,h5,h6,pre,.mu-divider{{margin:0;font-size:inherit;{MICRON_ROW_CSS}}}\
         {MICRON_FORM_CSS}\
         </style>\
         </head><body>{fragment}</body></html>"
    )
    .into_bytes()
}

/// Native form controls default to the platform's own light widget chrome,
/// which reads as broken against the forced-dark Micron page. Restyled to
/// match the surrounding grid rather than left at browser defaults.
const MICRON_FORM_CSS: &str = ".mu-field,.mu-submit{\
     font-family:inherit;font-size:inherit;line-height:1;\
     background:#232227;color:inherit;border:1px solid #4a4850;border-radius:3px;\
     padding:1px 4px}\
     .mu-submit{cursor:pointer}\
     .mu-submit:hover{background:#2d2c33}\
     .mu-field-label{cursor:pointer}\
     .mu-field-label input{margin-right:4px}";

/// Micron is a terminal format: NomadNet renders every line as one row of a
/// fixed character grid, so lines sit flush and spacing comes from the blank
/// lines the author wrote, not from margins.
///
/// Reproducing that grid is what makes multi-line ASCII banners hold together:
///
/// * `monospace` — block-drawing characters only tile if every cell is the
///   same width, and column alignment depends on it.
/// * `line-height:1` — a full-block glyph fills its em box, so anything above
///   1 leaves a horizontal seam between rows of a banner.
/// * `white-space:pre-wrap` — leading and interior runs of spaces carry the
///   shape of the art, and the default collapsing rules destroy them. `pre-wrap`
///   rather than `pre` because the reference wraps at the terminal width too,
///   and it avoids forcing horizontal scroll on ordinary prose.
///
/// Margins are zeroed (and heading sizes normalised to the grid) for the same
/// reason: a margin applies between every pair of lines, including the ones
/// inside a banner.
const MICRON_GRID_CSS: &str = "font-family:ui-monospace,\"DejaVu Sans Mono\",\"Liberation Mono\",Menlo,Consolas,monospace;\
     line-height:1";

/// Applied to the elements that hold a row's text, never to `body`.
///
/// `white-space` must not be inherited by the container: the fragment carries
/// a newline between each `</p>` and the next `<p>`, and preserving those
/// turns every gap between rows into a rendered blank line.
const MICRON_ROW_CSS: &str = "white-space:pre-wrap";
