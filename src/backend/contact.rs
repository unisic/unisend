//! Reading a received contact card.

use anyhow::{Result, anyhow};

use super::*;
use crate::vcard::{self, Card};

/// Downloads a `.vcf` attachment and reads its cards. Files over the size cap are not downloaded.
pub(super) async fn load(client: &Client, media: &Media) -> Result<Vec<Card>> {
    if media.size.is_some_and(|size| size > vcard::MAX_BYTES as u64) {
        return Err(anyhow!("contact file too large"));
    }
    let request = MediaRequestParameters { source: media.source.clone(), format: MediaFormat::File };
    let bytes = client.media().get_media_content(&request, true).await?;
    Ok(vcard::parse(&bytes))
}
