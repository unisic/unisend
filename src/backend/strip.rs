//! Metadata taken out of photos and videos before they leave the device: location, camera make and
//! model, maker notes, times, comments, embedded thumbnails. What is drawn or played stays byte for
//! byte; only what describes it goes. Every walk advances by at least one byte per step and reads
//! through bounds-checked slices, so a malformed file ends in an error, never a panic or a hang.
//!
//! Memory: a picture is copied once (the input and the output live together, at most `MAX_UPLOAD`
//! each); ISO media (MP4, MOV, HEIC, AVIF) is rewritten in place, with no copy.

use std::ops::Range;

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use image::metadata::Orientation;

/// A picture without metadata: JPEG, PNG, WebP, GIF, HEIC or AVIF, recognised by content.
pub fn image(data: Vec<u8>) -> Result<Vec<u8>> {
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        jpeg(&data)
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        png(&data)
    } else if data.len() >= 12 && data[..4] == *b"RIFF" && data[8..12] == *b"WEBP" {
        webp(&data)
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        gif(&data)
    } else if iso_like(&data) {
        let mut data = data;
        iso(&mut data)?;
        Ok(data)
    } else {
        bail!("not a picture this knows")
    }
}

/// An MP4, MOV or 3GP without location, device, times or timed-metadata tracks; nothing else is known.
pub fn video(data: &mut Vec<u8>) -> Result<()> {
    ensure!(iso_like(data), "not an ISO media file");
    iso(data)
}

fn get(d: &[u8], at: usize, n: usize) -> Result<&[u8]> {
    at.checked_add(n).and_then(|end| d.get(at..end)).ok_or_else(|| anyhow!("truncated at {at}"))
}
fn be16(d: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(get(d, at, 2)?.try_into()?))
}
fn be32(d: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(get(d, at, 4)?.try_into()?))
}
fn be64(d: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(get(d, at, 8)?.try_into()?))
}
fn le32(d: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(get(d, at, 4)?.try_into()?))
}
fn byte(d: &[u8], at: usize) -> Result<u8> {
    d.get(at).copied().ok_or_else(|| anyhow!("truncated at {at}"))
}

/// A TIFF block (as in EXIF) holding the orientation alone, when it turns the picture; dropping it
/// would show the picture on its side.
fn orientation_only(tiff: &[u8]) -> Option<Vec<u8>> {
    let turn = Orientation::from_exif_chunk(tiff)?.to_exif();
    (turn != 1).then(|| [&b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01"[..], &[0, turn, 0, 0, 0, 0, 0, 0]].concat())
}

/// Keeps APP0 (JFIF), APP14 (Adobe colour transform), the ICC profile and every coding segment;
/// drops EXIF and XMP (APP1), IPTC (APP13), MPF and other APP2, the other APPn, comments, and all
/// after the end marker (Samsung trailers, motion-photo videos, HDR gain maps).
fn jpeg(d: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(d.len());
    out.extend_from_slice(&d[..2]);
    let mut at = 2;
    let mut turned = false;
    loop {
        ensure!(byte(d, at)? == 0xff, "jpeg: no marker at {at}");
        while byte(d, at + 1)? == 0xff {
            at += 1;
        }
        let marker = byte(d, at + 1)?;
        if marker == 0xd9 {
            out.extend_from_slice(&[0xff, 0xd9]);
            return Ok(out);
        }
        if matches!(marker, 0x01 | 0xd0..=0xd7) {
            out.extend_from_slice(&d[at..at + 2]);
            at += 2;
            continue;
        }
        let len = usize::from(be16(d, at + 2)?);
        ensure!(len >= 2, "jpeg: segment length {len}");
        let end = at + 2 + len;
        let body = get(d, at + 4, len - 2)?;
        let keep = match marker {
            0xe0 | 0xee => true,
            0xe2 => body.starts_with(b"ICC_PROFILE\0"),
            0xe1 => {
                if let Some(tiff) = body.strip_prefix(b"Exif\0\0").filter(|_| !turned).and_then(orientation_only) {
                    out.extend_from_slice(&[0xff, 0xe1]);
                    out.extend_from_slice(&u16::try_from(tiff.len() + 8)?.to_be_bytes());
                    out.extend_from_slice(b"Exif\0\0");
                    out.extend_from_slice(&tiff);
                    turned = true;
                }
                false
            }
            0xe3..=0xef | 0xfe => false,
            _ => true,
        };
        if keep {
            out.extend_from_slice(&d[at..end]);
        }
        at = end;
        if marker == 0xda {
            // The entropy-coded scan runs to the next marker: 0xff is followed there by 0x00 (a
            // stuffed byte) or a restart marker, by anything else only at a real marker.
            let start = at;
            while !(byte(d, at)? == 0xff && !matches!(byte(d, at + 1)?, 0x00 | 0xd0..=0xd7)) {
                at += 1;
            }
            out.extend_from_slice(&d[start..at]);
        }
    }
}

/// CRC-32 as PNG chunks carry it.
fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    for &b in parts.iter().flat_map(|p| p.iter()) {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// Drops eXIf (save a turning orientation), tEXt, zTXt, iTXt, tIME and all after IEND.
fn png(d: &[u8]) -> Result<Vec<u8>> {
    let mut out = d[..8].to_vec();
    let mut at = 8;
    loop {
        let len = usize::try_from(be32(d, at)?)?;
        let kind = get(d, at + 4, 4)?;
        let end = at.checked_add(12).and_then(|e| e.checked_add(len)).filter(|&e| e <= d.len()).context("png: chunk past the end")?;
        match kind {
            b"eXIf" => {
                if let Some(tiff) = orientation_only(&d[at + 8..end - 4]) {
                    out.extend_from_slice(&u32::try_from(tiff.len())?.to_be_bytes());
                    out.extend_from_slice(b"eXIf");
                    out.extend_from_slice(&tiff);
                    out.extend_from_slice(&crc32(&[b"eXIf", &tiff]).to_be_bytes());
                }
            }
            b"tEXt" | b"zTXt" | b"iTXt" | b"tIME" => {}
            _ => out.extend_from_slice(&d[at..end]),
        }
        at = end;
        if kind == b"IEND" {
            return Ok(out);
        }
    }
}

/// Drops the EXIF (save a turning orientation) and XMP chunks, clears their VP8X flags, and drops
/// all after the RIFF end.
fn webp(d: &[u8]) -> Result<Vec<u8>> {
    const EXIF: u8 = 0x08;
    const XMP: u8 = 0x04;
    let riff_end = usize::try_from(le32(d, 4)?)?.checked_add(8).filter(|&e| e <= d.len()).context("webp: RIFF past the end")?;
    let mut out = d[..12].to_vec();
    let mut at = 12;
    let (mut vp8x, mut turn) = (None, None);
    while at < riff_end {
        let kind = get(d, at, 4)?;
        let len = usize::try_from(le32(d, at + 4)?)?;
        let body = at.checked_add(8).and_then(|b| b.checked_add(len)).filter(|&e| e <= riff_end).context("webp: chunk past the end")?;
        // Chunks are padded to even sizes; some writers leave the last pad out.
        let end = (body + (len & 1)).min(riff_end);
        match kind {
            b"EXIF" => turn = turn.or_else(|| orientation_only(d[at + 8..body].strip_prefix(b"Exif\0\0").unwrap_or(&d[at + 8..body]))),
            b"XMP " => {}
            _ => {
                if kind == b"VP8X" {
                    ensure!(len >= 1, "webp: empty VP8X");
                    vp8x = Some(out.len() + 8);
                }
                out.extend_from_slice(&d[at..end]);
            }
        }
        at = end;
    }
    if let Some(flags) = vp8x {
        out[flags] &= !(EXIF | XMP);
        if let Some(tiff) = turn {
            out[flags] |= EXIF;
            out.extend_from_slice(b"EXIF");
            out.extend_from_slice(&u32::try_from(tiff.len())?.to_le_bytes());
            out.extend_from_slice(&tiff);
        }
    }
    let size = u32::try_from(out.len() - 8)?;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Ok(out)
}

/// The end of a run of GIF sub-blocks starting at `at`.
fn gif_blocks(d: &[u8], mut at: usize) -> Result<usize> {
    loop {
        let n = usize::from(byte(d, at)?);
        at += 1 + n;
        if n == 0 {
            return Ok(at);
        }
    }
}

/// Size of a GIF colour table announced by `flags`.
fn gif_table(flags: u8) -> usize {
    if flags & 0x80 != 0 { 3 << ((flags & 7) + 1) } else { 0 }
}

/// Keeps the images, their control and plain-text extensions and the looping one (NETSCAPE2.0 or
/// ANIMEXTS1.0); drops comments, the other application extensions (XMP, ICC, makers') and all after
/// the trailer.
fn gif(d: &[u8]) -> Result<Vec<u8>> {
    let mut at = 13 + gif_table(byte(d, 10)?);
    let mut out = get(d, 0, at)?.to_vec();
    loop {
        match byte(d, at)? {
            0x3b => {
                out.push(0x3b);
                return Ok(out);
            }
            0x2c => {
                let start = at;
                at = gif_blocks(d, at + 10 + gif_table(byte(d, at + 9)?) + 1)?;
                out.extend_from_slice(get(d, start, at - start)?);
            }
            0x21 => {
                let label = byte(d, at + 1)?;
                let looping = label == 0xff && matches!(d.get(at + 3..at + 14), Some(b"NETSCAPE2.0" | b"ANIMEXTS1.0"));
                let start = at;
                at = gif_blocks(d, at + 2)?;
                if matches!(label, 0xf9 | 0x01) || looping {
                    out.extend_from_slice(get(d, start, at - start)?);
                }
            }
            other => bail!("gif: block 0x{other:02x} at {at}"),
        }
    }
}

fn iso_like(d: &[u8]) -> bool {
    matches!(d.get(4..8), Some(b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip"))
}

/// One ISO box: `head` ends its size and type, `body` its uuid user type when it has one.
#[derive(Clone, Copy)]
struct Bx {
    start: usize,
    head: usize,
    body: usize,
    end: usize,
    kind: [u8; 4],
}

/// The boxes from `at` to `to`; `None` past the first that does not fit, which is where the
/// boxes end and a trailer starts at the top level.
fn boxes(d: &[u8], mut at: usize, to: usize) -> (Vec<Bx>, Option<usize>) {
    let mut found = Vec::new();
    while at < to {
        let parsed = (|| -> Result<Bx> {
            ensure!(to - at >= 8, "iso: header past its parent");
            let kind: [u8; 4] = get(d, at + 4, 4)?.try_into()?;
            ensure!(kind.iter().all(|c| c.is_ascii_graphic() || *c == b' ' || *c == 0xa9), "iso: box type {kind:?}");
            let (size, head) = match be32(d, at)? {
                0 => (to - at, at + 8),
                1 => (usize::try_from(be64(d, at + 8)?)?, at + 16),
                n => (usize::try_from(n)?, at + 8),
            };
            ensure!(size >= head - at && size <= to - at, "iso: box size {size}");
            let body = if &kind == b"uuid" { head + 16 } else { head };
            ensure!(body <= at + size, "iso: uuid past its box");
            Ok(Bx { start: at, head, body, end: at + size, kind })
        })();
        match parsed {
            Ok(b) => {
                at = b.end;
                found.push(b);
            }
            Err(_) => return (found, Some(at)),
        }
    }
    (found, None)
}

/// The boxes inside `b`, which must all fit.
fn kids(d: &[u8], b: &Bx, skip: usize) -> Result<Vec<Bx>> {
    let from = b.body.checked_add(skip).filter(|&f| f <= b.end).context("iso: box too short")?;
    match boxes(d, from, b.end) {
        (found, None) => Ok(found),
        (_, Some(at)) => bail!("iso: bad box at {at} inside {:?}", b.kind),
    }
}

/// What the walk decided, applied once it is over.
enum Edit {
    /// The box becomes `free` with zeros inside: sizes and offsets elsewhere stay right.
    Free(Bx),
    Zero(Range<usize>),
    /// An EXIF item: an empty, valid EXIF block where it was.
    EmptyExif(Range<usize>),
}

/// The user type ISO 14496-12 gives XMP in a `uuid` box.
const XMP_UUID: [u8; 16] = [0xbe, 0x7a, 0xcf, 0xcb, 0x97, 0xa9, 0x42, 0xe8, 0x9c, 0x71, 0x99, 0x94, 0x91, 0xe3, 0xaf, 0xac];

fn xmp_box(d: &[u8], b: &Bx) -> bool {
    &b.kind == b"uuid" && d.get(b.head..b.body) == Some(&XMP_UUID[..])
}

/// MP4, MOV, 3GP, HEIC, AVIF: the movie's user data, metadata, XMP, times and timed-metadata
/// tracks, and a picture's EXIF and XMP items, wiped in place; a trailer after the last box (Samsung
/// keeps one) cut off.
fn iso(d: &mut Vec<u8>) -> Result<()> {
    let (top, trailer) = boxes(d, 0, d.len());
    ensure!(top.iter().any(|b| matches!(&b.kind, b"moov" | b"meta")), "iso: neither a movie nor a picture");
    let mut edits = Vec::new();
    for b in &top {
        match &b.kind {
            b"moov" => movie(d, b, &mut edits)?,
            b"meta" => items(d, b, &mut edits)?,
            b"udta" => edits.push(Edit::Free(*b)),
            _ if xmp_box(d, b) => edits.push(Edit::Free(*b)),
            _ => {}
        }
    }
    for edit in edits {
        match edit {
            Edit::Free(b) => {
                d[b.start + 4..b.start + 8].copy_from_slice(b"free");
                d[b.head..b.end].fill(0);
            }
            Edit::Zero(r) => d[r].fill(0),
            Edit::EmptyExif(r) => {
                const EMPTY: &[u8] = b"\0\0\0\0MM\0\x2a\0\0\0\x08\0\0\0\0\0\0";
                let n = EMPTY.len().min(r.len());
                d[r.start..r.start + n].copy_from_slice(&EMPTY[..n]);
                d[r.start + n..r.end].fill(0);
            }
        }
    }
    if let Some(at) = trailer {
        d.truncate(at);
    }
    Ok(())
}

/// Zeroes the creation and modification times of an mvhd, tkhd or mdhd.
fn times(d: &[u8], b: &Bx, edits: &mut Vec<Edit>) -> Result<()> {
    let n = if byte(d, b.body)? == 1 { 16 } else { 8 };
    ensure!(b.body + 4 + n <= b.end, "iso: short {:?}", b.kind);
    edits.push(Edit::Zero(b.body + 4..b.body + 4 + n));
    Ok(())
}

fn movie(d: &[u8], moov: &Bx, edits: &mut Vec<Edit>) -> Result<()> {
    let inside = kids(d, moov, 0)?;
    let fragmented = inside.iter().any(|b| &b.kind == b"mvex");
    for b in &inside {
        match &b.kind {
            b"mvhd" => times(d, b, edits)?,
            b"udta" | b"meta" => edits.push(Edit::Free(*b)),
            b"trak" => track(d, b, fragmented, edits)?,
            _ if xmp_box(d, b) => edits.push(Edit::Free(*b)),
            _ => {}
        }
    }
    Ok(())
}

/// Tracks that are pictures, sound or text; any other (timed metadata: GPS logs, camera motion,
/// Apple's `mebx`) is removed with its samples.
const PLAYED: [&[u8; 4]; 9] = [b"vide", b"soun", b"auxv", b"pict", b"subt", b"sbtl", b"text", b"clcp", b"tmcd"];

fn find<'a>(boxes: &'a [Bx], kind: &[u8; 4]) -> Option<&'a Bx> {
    boxes.iter().find(|b| &b.kind == kind)
}

fn track(d: &[u8], trak: &Bx, fragmented: bool, edits: &mut Vec<Edit>) -> Result<()> {
    let inside = kids(d, trak, 0)?;
    let media = find(&inside, b"mdia").map(|m| kids(d, m, 0)).transpose()?.unwrap_or_default();
    let handler = find(&media, b"hdlr").map(|h| get(d, h.body + 8, 4)).transpose()?;
    if !handler.is_some_and(|h| PLAYED.iter().any(|p| &p[..] == h)) {
        ensure!(!fragmented, "iso: a fragmented movie with a metadata track");
        let minf = find(&media, b"minf").map(|m| kids(d, m, 0)).transpose()?.unwrap_or_default();
        if let Some(stbl) = find(&minf, b"stbl") {
            samples(d, &kids(d, stbl, 0)?, edits)?;
        }
        edits.push(Edit::Free(*trak));
        return Ok(());
    }
    for b in &inside {
        match &b.kind {
            b"tkhd" => times(d, b, edits)?,
            b"udta" | b"meta" => edits.push(Edit::Free(*b)),
            _ => {}
        }
    }
    if let Some(mdhd) = find(&media, b"mdhd") {
        times(d, mdhd, edits)?;
    }
    Ok(())
}

/// Zeroes every sample a sample table points to, one range per chunk.
fn samples(d: &[u8], stbl: &[Bx], edits: &mut Vec<Edit>) -> Result<()> {
    let table = |kind: &[u8; 4]| find(stbl, kind).map(|b| &d[b.body..b.end]);
    let (Some(stsz), Some(stsc)) = (table(b"stsz"), table(b"stsc")) else {
        bail!("iso: a metadata track without stsz or stsc");
    };
    let offsets: Vec<u64> = if let Some(co) = table(b"stco") {
        (0..be32(co, 4)? as usize).map(|i| be32(co, 8 + 4 * i).map(u64::from)).collect::<Result<_>>()?
    } else if let Some(co) = table(b"co64") {
        (0..be32(co, 4)? as usize).map(|i| be64(co, 8 + 8 * i)).collect::<Result<_>>()?
    } else {
        bail!("iso: a metadata track without chunk offsets");
    };
    let fixed = u64::from(be32(stsz, 4)?);
    let count = u64::from(be32(stsz, 8)?);
    let runs = be32(stsc, 4)? as usize;
    let mut sample = 0u64;
    let mut prev = 0usize;
    for k in 0..runs {
        let first = be32(stsc, 8 + 12 * k)? as usize;
        let per = u64::from(be32(stsc, 12 + 12 * k)?);
        ensure!(first > prev, "iso: stsc out of order");
        prev = first;
        let next = if k + 1 < runs { be32(stsc, 8 + 12 * (k + 1))? as usize } else { offsets.len() + 1 };
        for chunk in first..next.min(offsets.len() + 1) {
            let n = per.min(count - sample);
            let len = if fixed != 0 {
                fixed.checked_mul(n).context("iso: chunk size")?
            } else {
                (sample..sample + n).map(|i| be32(stsz, 12 + 4 * i as usize).map(u64::from)).sum::<Result<u64>>()?
            };
            let start = offsets[chunk - 1];
            let end = start.checked_add(len).filter(|&e| e <= d.len() as u64).context("iso: sample past the end")?;
            edits.push(Edit::Zero(start as usize..end as usize));
            sample += n;
        }
    }
    Ok(())
}

/// A top-level `meta`: in a HEIF picture (it has items) the EXIF and XMP items are wiped; any
/// other is metadata alone and goes whole.
fn items(d: &[u8], meta: &Bx, edits: &mut Vec<Edit>) -> Result<()> {
    // ISO `meta` is a full box (4 bytes of version and flags first); QuickTime's is not.
    let inside = if get(d, meta.body + 8, 4).ok() == Some(&b"hdlr"[..]) { kids(d, meta, 4)? } else { kids(d, meta, 0)? };
    let (Some(iinf), Some(iloc)) = (find(&inside, b"iinf"), find(&inside, b"iloc")) else {
        edits.push(Edit::Free(*meta));
        return Ok(());
    };
    let mut exif = Vec::new();
    let mut xmp = Vec::new();
    let skip = if byte(d, iinf.body)? == 0 { 6 } else { 8 };
    for infe in kids(d, iinf, skip)?.iter().filter(|b| &b.kind == b"infe") {
        let version = byte(d, infe.body)?;
        if version < 2 {
            continue;
        }
        let (id, at) = if version == 2 { (u32::from(be16(d, infe.body + 4)?), infe.body + 8) } else { (be32(d, infe.body + 4)?, infe.body + 10) };
        let kind = get(d, at, 4)?;
        let rest = d.get(at + 4..infe.end).unwrap_or_default();
        if kind == b"Exif" {
            exif.push(id);
        } else if kind == b"mime" && rest.windows(19).any(|w| w == b"application/rdf+xml") {
            xmp.push(id);
        }
    }
    if exif.is_empty() && xmp.is_empty() {
        return Ok(());
    }
    let idat = find(&inside, b"idat").map(|b| b.body);
    let s = d.get(iloc.body..iloc.end).context("iso: iloc")?;
    let version = byte(s, 0)?;
    let sizes = byte(s, 4)?;
    let more = byte(s, 5)?;
    let (off_size, len_size, base_size) = (sizes >> 4, sizes & 15, more >> 4);
    let index_size = if version >= 1 { more & 15 } else { 0 };
    let uint = |at: usize, n: u8| -> Result<u64> {
        match n {
            0 => Ok(0),
            4 => be32(s, at).map(u64::from),
            8 => be64(s, at),
            n => bail!("iso: iloc field of {n} bytes"),
        }
    };
    let (count, mut at) = if version < 2 { (u64::from(be16(s, 6)?), 8) } else { (u64::from(be32(s, 6)?), 10) };
    for _ in 0..count {
        let id = if version < 2 { u32::from(be16(s, at)?) } else { be32(s, at)? };
        at += if version < 2 { 2 } else { 4 };
        let method = if version >= 1 {
            at += 2;
            be16(s, at - 2)? & 15
        } else {
            0
        };
        at += 2;
        let base = uint(at, base_size)?;
        at += usize::from(base_size);
        let extents = be16(s, at)?;
        at += 2;
        let wanted = exif.contains(&id) || xmp.contains(&id);
        let mut first = true;
        for _ in 0..extents {
            at += usize::from(index_size);
            let offset = uint(at, off_size)?;
            at += usize::from(off_size);
            let len = uint(at, len_size)?;
            at += usize::from(len_size);
            if !wanted {
                continue;
            }
            let origin = match method {
                0 => 0,
                1 => idat.context("iso: item in a missing idat")? as u64,
                m => bail!("iso: item construction method {m}"),
            };
            ensure!(len != 0, "iso: metadata item of open length");
            let start = origin.checked_add(base).and_then(|s| s.checked_add(offset)).context("iso: item offset")?;
            let end = start.checked_add(len).filter(|&e| e <= d.len() as u64).context("iso: item past the end")?;
            let range = start as usize..end as usize;
            edits.push(if exif.contains(&id) && first { Edit::EmptyExif(range) } else { Edit::Zero(range) });
            first = false;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageDecoder as _, RgbImage};

    use super::*;

    const GPS: &[u8] = b"GPS+52.2297+021.0122";

    fn picture() -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_fn(24, 16, |x, y| image::Rgb([(x * 10) as u8, (y * 15) as u8, 99])))
    }

    fn encoded(format: image::ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        picture().write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    fn has(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    fn exif(turn: u8) -> Vec<u8> {
        let mut tiff = b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
        tiff.extend_from_slice(&[0, turn, 0, 0, 0, 0, 0, 0]);
        tiff.extend_from_slice(GPS);
        tiff
    }

    fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
        [&[0xff, marker][..], &((body.len() + 2) as u16).to_be_bytes(), body].concat()
    }

    /// Every prefix and a few thousand byte flips: each ends in Ok or Err, none panics or hangs.
    fn survives(clean: impl Fn(Vec<u8>) -> Result<Vec<u8>>, sample: &[u8]) {
        for cut in 0..sample.len() {
            let _ = clean(sample[..cut].to_vec());
        }
        let mut seed = 0x2545_f491_u32;
        for _ in 0..3000 {
            let mut bad = sample.to_vec();
            for _ in 0..3 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let at = seed as usize % bad.len();
                bad[at] = (seed >> 24) as u8;
            }
            let _ = clean(bad);
        }
    }

    fn scan(jpeg: &[u8]) -> &[u8] {
        let sos = jpeg.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
        &jpeg[sos..]
    }

    fn sample_jpeg(turn: u8) -> Vec<u8> {
        let plain = encoded(image::ImageFormat::Jpeg);
        let mut out = plain[..2].to_vec();
        out.extend(segment(0xe1, &[&b"Exif\0\0"[..], &exif(turn)[..]].concat()));
        out.extend(segment(0xe1, &[&b"http://ns.adobe.com/xap/1.0/\0"[..], GPS].concat()));
        out.extend(segment(0xe2, b"ICC_PROFILE\0\x01\x01colours"));
        out.extend(segment(0xe2, &[&b"MPF\0"[..], GPS].concat()));
        out.extend(segment(0xed, &[&b"Photoshop 3.0\0"[..], GPS].concat()));
        out.extend(segment(0xfe, GPS));
        out.extend_from_slice(&plain[2..]);
        out.extend_from_slice(GPS);
        out
    }

    #[test]
    fn a_jpeg_keeps_its_colours_scan_and_turn_and_loses_the_rest() {
        let sample = sample_jpeg(6);
        let clean = image(sample.clone()).unwrap();
        assert!(!has(&clean, GPS), "metadata survived");
        assert!(has(&clean, b"ICC_PROFILE\0\x01\x01colours"));
        assert_eq!(scan(&clean), scan(&sample[..sample.len() - GPS.len()]), "pixels changed");
        let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(&clean)).unwrap();
        assert_eq!(decoder.orientation().unwrap(), Orientation::Rotate90);
        assert_eq!(decoder.dimensions(), (24, 16));

        let upright = image(sample_jpeg(1)).unwrap();
        assert!(!has(&upright, b"Exif"), "an upright picture needs no EXIF");
        assert_eq!(scan(&upright), scan(&clean));
    }

    #[test]
    fn a_progressive_jpegs_scans_and_restarts_survive() {
        // Two scans with a table between, a stuffed byte, a restart and fill bytes before a marker.
        let body = [0xff, 0xd8, 0xff, 0xdb, 0, 3, 7, 0xff, 0xda, 0, 3, 1, 0x12, 0xff, 0x00, 0x34, 0xff, 0xd0, 0x56, 0xff, 0xff, 0xc4, 0, 3, 9, 0xff, 0xda, 0, 3, 2, 0x78, 0xff, 0xd9];
        let mut with_comment = body[..7].to_vec();
        with_comment.extend(segment(0xfe, GPS));
        with_comment.extend_from_slice(&body[7..]);
        let mut expected = body.to_vec();
        expected.remove(19);
        assert_eq!(jpeg(&with_comment).unwrap(), expected);
    }

    #[test]
    fn a_png_loses_its_text_time_and_exif() {
        let plain = encoded(image::ImageFormat::Png);
        let chunk = |kind: &[u8], body: &[u8]| [&(body.len() as u32).to_be_bytes()[..], kind, body, &crc32(&[kind, body]).to_be_bytes()].concat();
        let mut sample = plain[..33].to_vec();
        sample.extend(chunk(b"tEXt", &[&b"Comment\0"[..], GPS].concat()));
        sample.extend(chunk(b"eXIf", &exif(3)));
        sample.extend(chunk(b"tIME", &[7, 0xe9, 1, 2, 3, 4, 5]));
        sample.extend(chunk(b"zTXt", GPS));
        sample.extend_from_slice(&plain[33..]);
        sample.extend_from_slice(GPS);
        let clean = image(sample.clone()).unwrap();
        assert!(!has(&clean, GPS) && !has(&clean, b"tIME") && !has(&clean, b"tEXt"));
        let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(&clean)).unwrap();
        assert_eq!(decoder.orientation().unwrap(), Orientation::Rotate180);
        assert_eq!(image::load_from_memory(&clean).unwrap().to_rgb8(), picture().to_rgb8());
        survives(image, &sample);
    }

    fn sample_webp() -> Vec<u8> {
        let mut lossless = Cursor::new(Vec::new());
        image::codecs::webp::WebPEncoder::new_lossless(&mut lossless).encode(picture().as_bytes(), 24, 16, image::ExtendedColorType::Rgb8).unwrap();
        let lossless = lossless.into_inner();
        let chunk = |kind: &[u8], body: &[u8]| {
            let pad: &[u8] = if body.len() % 2 == 1 { &[0] } else { &[] };
            [kind, &(body.len() as u32).to_le_bytes(), body, pad].concat()
        };
        let mut body = b"WEBP".to_vec();
        body.extend(chunk(b"VP8X", &[0x08 | 0x04, 0, 0, 0, 23, 0, 0, 15, 0, 0]));
        body.extend_from_slice(&lossless[12..]);
        body.extend(chunk(b"EXIF", &exif(8)));
        body.extend(chunk(b"XMP ", &[GPS, b"!"].concat()));
        [&b"RIFF"[..], &(body.len() as u32).to_le_bytes(), &body[..]].concat()
    }

    #[test]
    fn a_webp_loses_its_xmp_and_exif_but_the_turn() {
        let clean = image(sample_webp()).unwrap();
        assert!(!has(&clean, GPS) && !has(&clean, b"XMP "));
        assert_eq!(clean[20], 0x08, "VP8X keeps only the EXIF flag");
        assert_eq!(le32(&clean, 4).unwrap() as usize, clean.len() - 8);
        let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(&clean)).unwrap();
        assert_eq!(decoder.orientation().unwrap(), Orientation::Rotate270);
        assert_eq!(image::load_from_memory(&clean).unwrap().to_rgb8(), picture().to_rgb8());
        survives(image, &sample_webp());
    }

    #[test]
    fn a_gif_keeps_looping_and_loses_comments_and_xmp() {
        let mut plain = Cursor::new(Vec::new());
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut plain);
            encoder.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
            encoder.encode_frame(image::Frame::new(picture().to_rgba8())).unwrap();
        }
        let plain = plain.into_inner();
        let header = 13 + gif_table(plain[10]);
        let mut sample = plain[..header].to_vec();
        sample.extend_from_slice(&[0x21, 0xfe, GPS.len() as u8]);
        sample.extend_from_slice(GPS);
        sample.extend_from_slice(&[0, 0x21, 0xff, 11]);
        sample.extend_from_slice(b"XMP DataXMP");
        sample.extend_from_slice(&[GPS.len() as u8]);
        sample.extend_from_slice(GPS);
        sample.push(0);
        sample.extend_from_slice(&plain[header..]);
        sample.extend_from_slice(GPS);
        let clean = image(sample.clone()).unwrap();
        assert!(!has(&clean, GPS) && !has(&clean, b"XMP DataXMP"));
        assert!(has(&clean, b"NETSCAPE2.0"));
        assert_eq!(clean, plain, "only what was added is gone");
        survives(image, &sample);
    }

    #[test]
    fn broken_jpegs_end_in_an_error() {
        survives(image, &sample_jpeg(6));
        assert!(image(vec![0xff, 0xd8, 0xff, 0xe1, 0, 1]).is_err(), "a segment shorter than its length field");
        assert!(image(vec![0xff, 0xd8, 0xff, 0xda, 0, 2, 1, 2, 3]).is_err(), "a scan without an end");
        assert!(image(b"plain text".to_vec()).is_err());
    }

    fn bx(kind: &[u8], parts: &[&[u8]]) -> Vec<u8> {
        let body = parts.concat();
        [&((body.len() + 8) as u32).to_be_bytes()[..], kind, &body[..]].concat()
    }

    /// A movie with a video track, a GPS metadata track, user data and Apple-style metadata; its
    /// mdat holds the frame bytes, then the GPS sample.
    fn sample_movie() -> (Vec<u8>, Range<usize>) {
        let frames = b"FRAMEDATA-0123456789";
        let full = |v: u8| [v, 0, 0, 0];
        let stamped = [&full(0)[..], &0xdead_beef_u32.to_be_bytes(), &0xdead_beef_u32.to_be_bytes(), &[0; 12]].concat();
        let ftyp = bx(b"ftyp", &[b"isom", &[0, 0, 2, 0], b"isommp42"]);
        let build = |mdat_at: u32| {
            let gps_at = mdat_at + 8 + frames.len() as u32;
            let video = bx(b"trak", &[&bx(b"tkhd", &[&stamped]), &bx(b"mdia", &[&bx(b"mdhd", &[&stamped]), &bx(b"hdlr", &[&full(0), &[0; 4], b"vide", &[0; 13]])])]);
            let stbl = bx(b"stbl", &[
                &bx(b"stsz", &[&full(0), &0u32.to_be_bytes(), &1u32.to_be_bytes(), &(GPS.len() as u32).to_be_bytes()]),
                &bx(b"stsc", &[&full(0), &1u32.to_be_bytes(), &1u32.to_be_bytes(), &1u32.to_be_bytes(), &1u32.to_be_bytes()]),
                &bx(b"stco", &[&full(0), &1u32.to_be_bytes(), &gps_at.to_be_bytes()]),
            ]);
            let gps = bx(b"trak", &[&bx(b"tkhd", &[&stamped]), &bx(b"mdia", &[&bx(b"hdlr", &[&full(0), &[0; 4], b"meta", &[0; 13]]), &bx(b"minf", &[&stbl])])]);
            let udta = bx(b"udta", &[&bx(b"\xa9xyz", &[GPS]), &bx(b"\xa9mod", &[b"SM-S911B"])]);
            let meta = bx(b"meta", &[&bx(b"hdlr", &[&[0; 8], b"mdta", &[0; 13]]), &bx(b"keys", &[b"com.apple.quicktime.location.ISO6709"]), &bx(b"ilst", &[GPS])]);
            bx(b"moov", &[&bx(b"mvhd", &[&stamped, &[0; 80]]), &video, &gps, &udta, &meta])
        };
        let mdat_at = (ftyp.len() + build(0).len()) as u32;
        let moov = build(mdat_at);
        let mdat = bx(b"mdat", &[frames, GPS]);
        let file = [ftyp, moov, mdat].concat();
        let frames_at = mdat_at as usize + 8;
        (file, frames_at..frames_at + frames.len())
    }

    #[test]
    fn a_movie_loses_its_location_device_times_and_gps_track_and_keeps_its_frames() {
        let (sample, frames) = sample_movie();
        let mut clean = sample.clone();
        video(&mut clean).unwrap();
        assert_eq!(clean.len(), sample.len(), "sizes and offsets stay");
        assert_eq!(clean[frames.clone()], sample[frames]);
        for gone in [GPS, b"SM-S911B", b"com.apple.quicktime", b"\xde\xad\xbe\xef", b"\xa9xyz", b"meta"] {
            assert!(!has(&clean, gone), "{} survived", String::from_utf8_lossy(gone));
        }
        assert!(has(&clean, b"vide") && has(&clean, b"moov"));

        let mut trailed = [&sample[..], b"\0\x01\x02\x03SEFHSEFT", GPS].concat();
        video(&mut trailed).unwrap();
        assert_eq!(trailed.len(), sample.len(), "the trailer is cut");

        survives(|d| image(d.clone()).or_else(|_| { let mut d = d; video(&mut d).map(|()| d) }), &sample);
        assert!(video(&mut b"\x1a\x45\xdf\xa3webm".to_vec()).is_err());
    }

    #[test]
    fn a_heif_picture_loses_its_exif_and_xmp_items() {
        let pixels = b"HEVC-PIXELS-0123";
        let xmp = [&b"<x:xmpmeta>"[..], GPS].concat();
        let exif_item = [&[0, 0, 0, 6][..], b"Exif\0\0", &exif(1)[..]].concat();
        let infe = |id: u16, kind: &[u8], extra: &[u8]| bx(b"infe", &[&[2, 0, 0, 0], &id.to_be_bytes(), &[0, 0], kind, b"\0", extra]);
        let build = |mdat_at: u32| {
            let at = mdat_at + 8;
            let item = |id: u16, off: u32, len: usize| [&id.to_be_bytes()[..], &[0, 0], &[0, 1], &off.to_be_bytes(), &(len as u32).to_be_bytes()].concat();
            let iloc = bx(b"iloc", &[&[0, 0, 0, 0], &[0x44, 0x00], &3u16.to_be_bytes(),
                &item(1, at, pixels.len()), &item(2, at + pixels.len() as u32, exif_item.len()), &item(3, at + (pixels.len() + exif_item.len()) as u32, xmp.len())]);
            let iinf = bx(b"iinf", &[&[0, 0, 0, 0], &3u16.to_be_bytes(), &infe(1, b"hvc1", b""), &infe(2, b"Exif", b""), &infe(3, b"mime", b"application/rdf+xml\0")]);
            bx(b"meta", &[&[0, 0, 0, 0], &bx(b"hdlr", &[&[0; 8], b"pict", &[0; 13]]), &bx(b"pitm", &[&[0, 0, 0, 0], &[0, 1]]), &iinf, &iloc])
        };
        let ftyp = bx(b"ftyp", &[b"heic", &[0; 4], b"mif1heic"]);
        let mdat_at = (ftyp.len() + build(0).len()) as u32;
        let sample = [ftyp, build(mdat_at), bx(b"mdat", &[pixels, &exif_item, &xmp])].concat();
        let clean = image(sample.clone()).unwrap();
        assert_eq!(clean.len(), sample.len());
        assert!(!has(&clean, GPS) && !has(&clean, b"xmpmeta"));
        assert!(has(&clean, pixels));
        let exif_at = mdat_at as usize + 8 + pixels.len();
        assert_eq!(&clean[exif_at..exif_at + 18], b"\0\0\0\0MM\0\x2a\0\0\0\x08\0\0\0\0\0\0");
        survives(image, &sample);
    }
}
