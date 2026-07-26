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
pub async fn fetch(
    handle: &ReticulumHandle,
    identity: &Identity,
    target_identity_hash: [u8; 16],
    path: &str,
    timeout: Duration,
) -> Result<FetchedContent, NomadFetchError> {
    let link_client = LinkClient::new(handle.transport_tx.clone(), identity.clone());
    let bytes = link_client
        .query(
            target_identity_hash,
            nomad_core::announce::APP_NAME,
            path,
            Vec::new(),
            1,
            timeout,
        )
        .await?;
    Ok(classify(path, bytes))
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
}
