//! The original file of an attachment, downloaded for the viewer's Save and Share buttons.

use std::path::PathBuf;

use matrix_sdk::Client;
use tokio::sync::mpsc;

use super::{Ev, Media, MediaFormat, MediaRequestParameters, safe_name, write_private};

/// What the user wants done with the file once it is here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Export {
    Save,
    Share,
}

/// Downloads into the media cache, one directory per event as `Cmd::Fetch` does, and says where.
pub(super) async fn run(client: Client, ev: mpsc::Sender<Ev>, dir: PathBuf, id: String, media: Media, what: Export) {
    let saved = async {
        let request = MediaRequestParameters { source: media.source.clone(), format: MediaFormat::File };
        let bytes = client.media().get_media_content(&request, false).await?;
        let folder = dir.join(safe_name(&id));
        std::fs::create_dir_all(&folder)?;
        let path = folder.join(safe_name(&media.name));
        write_private(&path, &bytes)?;
        anyhow::Ok(path)
    };
    let path = match saved.await {
        Ok(path) => Some(path),
        Err(e) => {
            let _ = ev.send(Ev::Error(format!("Could not download {}: {e:#}", media.name))).await;
            None
        }
    };
    let _ = ev.send(Ev::Exported(id, path, what)).await;
}
