use std::time::Duration;

use rns_identity::identity::Identity;
use rns_runtime::link_client::{LinkClient, LinkClientError};
use rns_runtime::reticulum::ReticulumHandle;

#[derive(Debug, thiserror::Error)]
pub enum NomadFetchError {
    #[error(transparent)]
    Link(#[from] LinkClientError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchedContent {
    Html(Vec<u8>),
    Micron(Vec<u8>),
    File { name: String, bytes: Vec<u8> },
}

/// One-shot fetch of a `nomadnetwork.node` page or file, over the app's
/// existing Reticulum runtime. Does not reimplement the link handshake —
/// `LinkClient::query` already does path discovery + link + identify + request.
/// `payload` is the request data (e.g. a form submission — see
/// `encode_form_payload`); pass an empty `Vec` for an ordinary fetch.
pub async fn fetch(
    handle: &ReticulumHandle,
    identity: &Identity,
    target_identity_hash: [u8; 16],
    path: &str,
    payload: Vec<u8>,
    timeout: Duration,
) -> Result<FetchedContent, NomadFetchError> {
    let link_client = LinkClient::new(handle.transport_tx.clone(), identity.clone());
    let bytes = link_client
        .query(
            target_identity_hash,
            nomad_core::announce::APP_NAME,
            path,
            payload,
            1,
            timeout,
        )
        .await?;
    Ok(classify(path, bytes))
}

/// Same as [`fetch`], but sends `(bytes_received, total_bytes)` on `progress`
/// as the reply's resource segments arrive. See
/// `LinkClient::query_with_progress` for when `progress` does and doesn't fire.
pub async fn fetch_with_progress(
    handle: &ReticulumHandle,
    identity: &Identity,
    target_identity_hash: [u8; 16],
    path: &str,
    payload: Vec<u8>,
    timeout: Duration,
    progress: tokio::sync::mpsc::UnboundedSender<(usize, usize)>,
) -> Result<FetchedContent, NomadFetchError> {
    let link_client = LinkClient::new(handle.transport_tx.clone(), identity.clone());
    let bytes = link_client
        .query_with_progress(
            target_identity_hash,
            nomad_core::announce::APP_NAME,
            path,
            payload,
            1,
            timeout,
            progress,
        )
        .await?;
    Ok(classify(path, bytes))
}

/// Encodes a `nomad://` request's query string (from a native HTML
/// `<form method="get">` submission, no JavaScript involved) into the
/// msgpack map nodepage-rs's `decode_fields` (`nomad-core/src/script.rs`)
/// expects: `{"field_<name>": "<value>", ...}`, string keys and values. Our
/// rendered `<input name="field_<name>">` (see `micron.rs`) already carries
/// that exact key shape, matching what NomadNet's own `Browser.py` builds
/// (`request_data["field_" + name] = ...`) — so query keys need no
/// client-side renaming, only decoding and re-encoding as msgpack.
///
/// Multiple values under the same key — a checkbox group sharing a `name`,
/// which a native form serializes as repeated `key=value` pairs — are
/// comma-joined, matching `Browser.py`'s own `CheckBox` handling
/// (`existing_value + ',' + user_data`) exactly.
///
/// Returns an empty `Vec` for an empty or field-less query string, so an
/// ordinary navigation with no form data behaves exactly as before (no
/// payload at all, not an empty encoded map).
pub fn encode_form_payload(query: &str) -> Vec<u8> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key);
        let value = percent_decode(value);
        match fields.iter_mut().find(|(k, _)| *k == key) {
            Some((_, existing)) => {
                existing.push(',');
                existing.push_str(&value);
            }
            None => fields.push((key, value)),
        }
    }

    if fields.is_empty() {
        return Vec::new();
    }

    let entries = fields
        .into_iter()
        .map(|(k, v)| (rmpv::Value::from(k), rmpv::Value::from(v)))
        .collect();
    let mut buf = Vec::new();
    let _ = rmpv::encode::write_value(&mut buf, &rmpv::Value::Map(entries));
    buf
}

/// Decodes one `application/x-www-form-urlencoded` component: `+` is a
/// literal space, `%XX` is a hex-escaped byte — exactly what a native form's
/// own GET-query serialization produces, so this only ever sees well-formed
/// input in practice.
fn percent_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut iter = s.bytes();
    while let Some(b) = iter.next() {
        match b {
            b'+' => bytes.push(b' '),
            b'%' => match (iter.next(), iter.next()) {
                (Some(h1), Some(h2)) => {
                    let hex = [h1 as char, h2 as char];
                    match u8::from_str_radix(&hex.iter().collect::<String>(), 16) {
                        Ok(byte) => bytes.push(byte),
                        Err(_) => bytes.push(b'%'),
                    }
                }
                _ => bytes.push(b'%'),
            },
            other => bytes.push(other),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn classify(path: &str, bytes: Vec<u8>) -> FetchedContent {
    let ext = path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase());
    match ext.as_deref() {
        Some("html") | Some("htm") => FetchedContent::Html(bytes),
        Some("mu") => FetchedContent::Micron(bytes),
        _ => {
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            FetchedContent::File { name, bytes }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_html_by_extension() {
        assert_eq!(
            classify("/page/index.html", b"hi".to_vec()),
            FetchedContent::Html(b"hi".to_vec())
        );
        assert_eq!(
            classify("/page/index.htm", b"hi".to_vec()),
            FetchedContent::Html(b"hi".to_vec())
        );
    }

    #[test]
    fn classifies_micron_by_extension() {
        assert_eq!(
            classify("/page/index.mu", b"hi".to_vec()),
            FetchedContent::Micron(b"hi".to_vec())
        );
    }

    #[test]
    fn classifies_uppercase_extension() {
        assert_eq!(
            classify("/page/INDEX.HTML", b"hi".to_vec()),
            FetchedContent::Html(b"hi".to_vec())
        );
    }

    #[test]
    fn classifies_anything_else_as_file_with_last_segment_as_name() {
        assert_eq!(
            classify("/file/report.pdf", b"data".to_vec()),
            FetchedContent::File {
                name: "report.pdf".to_string(),
                bytes: b"data".to_vec(),
            }
        );
    }

    #[test]
    fn classifies_extensionless_path_as_file() {
        assert_eq!(
            classify("/page/index", b"data".to_vec()),
            FetchedContent::File {
                name: "index".to_string(),
                bytes: b"data".to_vec(),
            }
        );
    }

    fn decode_payload(bytes: &[u8]) -> rmpv::Value {
        rmpv::decode::read_value(&mut &bytes[..]).expect("valid msgpack")
    }

    #[test]
    fn empty_query_encodes_to_no_payload_at_all() {
        assert_eq!(encode_form_payload(""), Vec::<u8>::new());
    }

    #[test]
    fn encodes_a_single_field_as_a_msgpack_map() {
        let payload = encode_form_payload("field_username=alice");
        let value = decode_payload(&payload);
        assert_eq!(
            value,
            rmpv::Value::Map(vec![(
                rmpv::Value::from("field_username"),
                rmpv::Value::from("alice")
            )])
        );
    }

    #[test]
    fn percent_decodes_keys_and_values() {
        let payload = encode_form_payload("field_full%20name=Jane%20Doe%2C%20Jr.");
        let value = decode_payload(&payload);
        assert_eq!(
            value,
            rmpv::Value::Map(vec![(
                rmpv::Value::from("field_full name"),
                rmpv::Value::from("Jane Doe, Jr.")
            )])
        );
    }

    #[test]
    fn plus_decodes_to_a_literal_space() {
        let payload = encode_form_payload("field_name=Jane+Doe");
        let value = decode_payload(&payload);
        assert_eq!(
            value,
            rmpv::Value::Map(vec![(
                rmpv::Value::from("field_name"),
                rmpv::Value::from("Jane Doe")
            )])
        );
    }

    /// A native form serializes a checkbox group sharing one `name` as
    /// repeated `key=value` pairs; Browser.py comma-joins them under that one
    /// key, and this must match exactly.
    #[test]
    fn repeated_keys_comma_join_like_a_checkbox_group() {
        let payload = encode_form_payload("field_colors=red&field_colors=blue&field_colors=green");
        let value = decode_payload(&payload);
        assert_eq!(
            value,
            rmpv::Value::Map(vec![(
                rmpv::Value::from("field_colors"),
                rmpv::Value::from("red,blue,green")
            )])
        );
    }

    #[test]
    fn multiple_distinct_fields_all_present() {
        let payload = encode_form_payload("field_username=alice&field_password=hunter2");
        let value = decode_payload(&payload);
        assert_eq!(
            value,
            rmpv::Value::Map(vec![
                (rmpv::Value::from("field_username"), rmpv::Value::from("alice")),
                (rmpv::Value::from("field_password"), rmpv::Value::from("hunter2")),
            ])
        );
    }
}
