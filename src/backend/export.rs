//! The original file of an attachment, downloaded for the viewer's Save and Share buttons.

use std::path::PathBuf;

use matrix_sdk::Client;
use tokio::sync::mpsc;

use crate::i18n::t;

use super::{Ev, Media, MediaFormat, MediaRequestParameters, save_media};

/// What the user wants done with the file once it is here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Export {
    Save,
    Share,
}

/// Downloads into the media cache, one directory per event as `Cmd::Fetch` does, and says where.
pub(super) async fn run(client: Client, ev: mpsc::Sender<Ev>, dir: PathBuf, generation: u64, id: String, media: Media, what: Export) {
    let saved = async {
        let request = MediaRequestParameters { source: media.source.clone(), format: MediaFormat::File };
        let bytes = client.media().get_media_content(&request, false).await?;
        save_media(&dir, generation, &id, &media.name, &bytes)
    };
    let path = match saved.await {
        Ok(path) => Some(path),
        Err(e) => {
            let _ = ev.send(Ev::Error(t!("Could not download {name}: {error}", name = media.name, error = format!("{e:#}")))).await;
            None
        }
    };
    let _ = ev.send(Ev::Exported(id, path, what)).await;
}
