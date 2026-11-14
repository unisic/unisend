//! A colour emoji font of the user's own in place of the bundled or system one, where GPUI draws
//! text itself (Linux and Android; macOS and iOS shape with CoreText, which uses the system's).
//!
//! GPUI's text system draws a glyph in colour only from a face whose PostScript name is
//! "NotoColorEmoji", cosmic-text's own fallback looks for the family "Noto Color Emoji" on Linux,
//! and GPUI drops a face without a Latin 'm' when its family is requested. So the copy kept here
//! is renamed to those names and given an 'm', which lets it also be the root text style's
//! fallback family (see `fallbacks`).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use fontdb::{Database, Language, Source};

use crate::Settings;

const FILE: &str = "emoji-font";
const PART: &str = "emoji-font.part";
const LIMIT: u64 = 300 * 1024 * 1024;
const FAMILY: &str = "Noto Color Emoji";
const POSTSCRIPT: &str = "NotoColorEmoji";

fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// Whether this platform draws emoji through cosmic-text, so a font of one's own can be used.
pub(crate) const SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "android"));

/// Puts the chosen font in place of the bundled or system one. `false` when none is chosen or
/// it cannot be read; then the choice is cleared and the caller sets up the default.
pub(crate) fn apply(settings: &mut Settings, dir: &Path, cx: &mut gpui_kit::App) -> bool {
    if !SUPPORTED || settings.emoji_font.is_none() {
        return false;
    }
    let Some(bytes) = load(dir) else {
        log::warn!("emoji font: missing or unreadable, using the system's");
        settings.emoji_font = None;
        settings.save(dir);
        let _ = fs::remove_file(path(dir));
        return false;
    };
    // Requesting the family drops every face of it (none has an 'm'): the bundled or system
    // emoji font goes, and ours is the only one by that name.
    let _ = cx.text_system().resolve_font(&gpui_kit::font(FAMILY));
    if let Err(e) = cx.text_system().add_fonts(vec![bytes.into()]) {
        log::warn!("emoji font: {e}");
        return false;
    }
    let _ = CHAIN.set(gpui_kit::FontFallbacks::from_fonts(vec![FAMILY.into()]));
    true
}

/// Makes the APK's bundled copy (already named and given an 'm', see `assets/fonts/README`) the
/// emoji family. On Android that holds only when the system's own face is not CBDT: then
/// gpui-mobile loads the system face instead, and requesting the family would drop it.
pub(crate) fn use_bundled() {
    #[cfg(target_os = "android")]
    if has_cbdt(Path::new("/system/fonts/NotoColorEmoji.ttf")) {
        return;
    }
    let _ = CHAIN.set(gpui_kit::FontFallbacks::from_fonts(vec![FAMILY.into()]));
}

#[cfg(target_os = "android")]
fn has_cbdt(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else { return false };
    let Ok(head) = read_at(&mut file, 0, 12) else { return false };
    let count = usize::from(be16(&head, 4).unwrap_or(0));
    read_at(&mut file, 12, 16 * count).is_ok_and(|records| records.chunks(16).any(|r| r.starts_with(b"CBDT")))
}

static CHAIN: OnceLock<gpui_kit::FontFallbacks> = OnceLock::new();

/// For the root text style, once the user's own font is in: it then draws every grapheme whose
/// first character the app's face lacks, before cosmic-text's fallback list, whose text fonts
/// come first (on Linux U+263A U+FE0F is otherwise a white glyph from Noto Sans Symbols). GPUI
/// keeps a grapheme on the app's face when it has the first character: keycaps (ASCII) and, where
/// the system's Inter 4 is the face picked, U+2764.
pub(crate) fn fallbacks() -> Option<gpui_kit::FontFallbacks> {
    CHAIN.get().cloned()
}

/// The kept font, mapped for the life of the process: only the pages of glyphs drawn are read.
fn load(dir: &Path) -> Option<&'static [u8]> {
    let map = map(&path(dir))?;
    if !usable(&map) {
        return None;
    }
    let map = Arc::try_unwrap(map).ok()?;
    Some(&Box::leak(Box::new(map))[..])
}

fn map(path: &Path) -> Option<Arc<memmap2::Mmap>> {
    let file = File::open(path).ok()?;
    // Safety: the kept file is only ever replaced by renaming another over it or removed, never
    // written in place, so the mapped pages cannot change under the text system. A file being
    // prepared is written only after its mapping is dropped.
    Some(Arc::new(unsafe { memmap2::Mmap::map(&file) }.ok()?))
}

/// Whether every face has the names and the 'm' that `rename` gives: without the 'm', requesting
/// the family would drop the face.
fn usable(map: &Arc<memmap2::Mmap>) -> bool {
    let faces = faces(Source::Binary(map.clone()));
    let m = ttf_parser::Face::parse(map, 0).is_ok_and(|face| face.glyph_index('m').is_some());
    m && !faces.is_empty() && faces.iter().all(|(family, ps)| family == FAMILY && ps == POSTSCRIPT)
}

/// Each face's family name (English if it has one) and PostScript name, as the text system reads them.
fn faces(source: Source) -> Vec<(String, String)> {
    let mut db = Database::new();
    db.load_font_source(source);
    db.faces()
        .map(|face| {
            let english = face.families.iter().find(|(_, lang)| *lang == Language::English_UnitedStates);
            let family = english.or(face.families.first()).map(|(name, _)| name.clone()).unwrap_or_default();
            (family, face.post_script_name.clone())
        })
        .collect()
}

/// Copies the font at `src` into `dir` in place of any earlier choice and returns its family
/// name, or a sentence saying why it cannot be used.
pub(crate) fn import(src: &Path, dir: &Path) -> Result<String, String> {
    let size = fs::metadata(src).map_err(|_| "The file cannot be read.".to_owned())?.len();
    if size > LIMIT {
        return Err("The file is larger than 300 MB.".into());
    }
    let part = dir.join(PART);
    let made = place(src, &part).and_then(|()| prepare(&part));
    let kept = made.and_then(|family| fs::rename(&part, path(dir)).map(|()| family).map_err(|_| "The font cannot be saved.".to_owned()));
    if kept.is_err() {
        let _ = fs::remove_file(&part);
    }
    kept
}

/// Stops using the chosen font from the next start.
pub(crate) fn remove(dir: &Path) {
    if let Err(e) = fs::remove_file(path(dir))
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("emoji font: {e}");
    }
}

fn place(src: &Path, part: &Path) -> Result<(), String> {
    // Android's picker already copied the file into the app's cache, on the same disk.
    #[cfg(target_os = "android")]
    if fs::rename(src, part).is_ok() {
        return Ok(());
    }
    fs::copy(src, part).map(drop).map_err(|_| "The file cannot be read.".to_owned())
}

fn prepare(part: &Path) -> Result<String, String> {
    let not_font = || "Not a font file.".to_owned();
    let family = map(part).map_or_else(Vec::new, |map| faces(Source::Binary(map))).into_iter().next().map(|(family, _)| family).ok_or_else(not_font)?;
    let mut file = OpenOptions::new().read(true).write(true).open(part).map_err(|_| not_font())?;
    rename(&mut file)?;
    file.sync_all().map_err(|_| "The font cannot be saved.".to_owned())?;
    if !map(part).is_some_and(|map| usable(&map)) {
        return Err(not_font());
    }
    Ok(family)
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn read_at(file: &mut File, at: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0; len];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_at(file: &mut File, at: u64, bytes: &[u8]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(at))?;
    file.write_all(bytes)
}

/// Gives the font the names the text system knows (see the module comment) and a Latin 'm', so
/// it can be a requested fallback family: new `name` and `cmap` tables are appended and the
/// face's table directory pointed at them. A collection keeps its first face.
fn rename(file: &mut File) -> Result<(), String> {
    let not_font = || "Not a font file.".to_owned();
    let io = |_| not_font();
    let head = read_at(file, 0, 16).map_err(io)?;
    let collection = head.starts_with(b"ttcf");
    let face = if collection { be32(&head, 12).ok_or_else(not_font)? } else { 0 };
    let count = be16(&read_at(file, face.into(), 12).map_err(io)?, 4).ok_or_else(not_font)?;
    let records = read_at(file, u64::from(face) + 12, 16 * usize::from(count)).map_err(io)?;
    let table = |tag: &[u8]| (0..usize::from(count)).map(|i| i * 16).find(|&r| &records[r..r + 4] == tag);
    let mut read_table = |r: usize| -> Result<Vec<u8>, String> {
        let (offset, len) = (be32(&records, r + 8).ok_or_else(not_font)?, be32(&records, r + 12).ok_or_else(not_font)?);
        read_at(file, offset.into(), len as usize).map_err(io)
    };
    let colr_v0 = match table(b"COLR") {
        Some(r) => be16(&read_table(r)?, 0) == Some(0),
        None => false,
    };
    if table(b"sbix").is_none() && (table(b"CBDT").is_none() || table(b"CBLC").is_none()) && !colr_v0 {
        return Err(if table(b"COLR").is_some() {
            "This font's colour format (COLRv1) cannot be drawn yet. Choose one with bitmap emoji.".into()
        } else {
            "Not a colour emoji font.".into()
        });
    }
    let (names, cmap) = (table(b"name").ok_or_else(not_font)?, table(b"cmap").ok_or_else(not_font)?);
    let renamed = renamed(&read_table(names)?).ok_or_else(not_font)?;
    let with_m = with_m(&read_table(cmap)?).ok_or_else(|| "Not a colour emoji font.".to_owned())?;
    for (r, bytes) in [(names, renamed), (cmap, with_m)] {
        append_table(file, u64::from(face) + 12 + r as u64, bytes).map_err(io)?;
    }
    if collection {
        // Version 1, one font: the other faces keep their own names, which the text system
        // would otherwise find as fonts it does not draw in colour.
        write_at(file, 4, &[0, 1, 0, 0, 0, 0, 0, 1]).map_err(io)?;
    }
    Ok(())
}

/// Writes `table` at the end of the file and points the table record at `record` to it.
fn append_table(file: &mut File, record: u64, mut table: Vec<u8>) -> std::io::Result<()> {
    let too_big = |_| std::io::Error::other("font too large");
    let end = file.seek(SeekFrom::End(0))?;
    let at = end.next_multiple_of(4);
    let len = u32::try_from(table.len()).map_err(too_big)?;
    table.resize(table.len().next_multiple_of(4), 0);
    let mut padded = vec![0; (at - end) as usize];
    padded.extend_from_slice(&table);
    write_at(file, end, &padded)?;
    let mut entry = checksum(&table).to_be_bytes().to_vec();
    entry.extend(u32::try_from(at).map_err(too_big)?.to_be_bytes());
    entry.extend(len.to_be_bytes());
    write_at(file, record + 4, &entry)
}

/// The `cmap` table with 'm' added to its full-Unicode (format 12) subtables, mapped to the
/// space's glyph. GPUI drops a face without 'm' when its family is requested; the 'm' itself is
/// never drawn from this font, as GPUI keeps ASCII on the primary font. `None` without format 12.
fn with_m(table: &[u8]) -> Option<Vec<u8>> {
    const M: u32 = 'm' as u32;
    let count = usize::from(be16(table, 2)?);
    let header = 4 + 8 * count;
    let mut out = table.get(..header)?.to_vec();
    let mut moved: Vec<(u32, u32)> = Vec::new();
    let mut patched = false;
    for i in 0..count {
        let r = 4 + 8 * i;
        let old = be32(table, r + 4)?;
        let new = match moved.iter().find(|(o, _)| *o == old) {
            Some(&(_, new)) => new,
            None => {
                let at = old as usize;
                let format = be16(table, at)?;
                let len = match format {
                    8 | 10 | 12 | 13 => be32(table, at + 4)? as usize,
                    14 => be32(table, at + 2)? as usize,
                    _ => usize::from(be16(table, at + 2)?),
                };
                let mut sub = table.get(at..at + len)?.to_vec();
                if format == 12 {
                    let groups = be32(&sub, 12)? as usize;
                    let group = |i: usize| Some((be32(&sub, 16 + 12 * i)?, be32(&sub, 20 + 12 * i)?, be32(&sub, 24 + 12 * i)?));
                    let all = (0..groups).map(group).collect::<Option<Vec<_>>>()?;
                    if !all.iter().any(|&(start, end, _)| (start..=end).contains(&M)) {
                        let space = all.iter().find(|&&(start, end, _)| (start..=end).contains(&0x20)).map_or(1, |&(start, _, glyph)| glyph + 0x20 - start);
                        let ix = all.partition_point(|&(start, _, _)| start < M);
                        let bytes: Vec<u8> = [M, M, space].iter().flat_map(|v| v.to_be_bytes()).collect();
                        sub.splice(16 + 12 * ix..16 + 12 * ix, bytes);
                        let len = u32::try_from(sub.len()).ok()?;
                        sub[4..8].copy_from_slice(&len.to_be_bytes());
                        sub[12..16].copy_from_slice(&u32::try_from(groups + 1).ok()?.to_be_bytes());
                    }
                    patched = true;
                }
                out.resize(out.len().next_multiple_of(4), 0);
                let new = u32::try_from(out.len()).ok()?;
                out.extend(sub);
                moved.push((old, new));
                new
            }
        };
        out.get_mut(r + 4..r + 8)?.copy_from_slice(&new.to_be_bytes());
    }
    patched.then_some(out)
}

/// The `name` table with the family, full and PostScript names replaced. The old strings stay;
/// the replacements are added after them.
fn renamed(table: &[u8]) -> Option<Vec<u8>> {
    let count = usize::from(be16(table, 2)?);
    let storage = usize::from(be16(table, 4)?);
    let base = table.len().checked_sub(storage)?;
    let mut out = table.to_vec();
    let mut extra = Vec::new();
    for i in 0..count {
        let r = 6 + 12 * i;
        let (platform, encoding, id) = (be16(table, r)?, be16(table, r + 2)?, be16(table, r + 6)?);
        let text = match id {
            1 | 4 | 16 | 21 => FAMILY,
            6 => POSTSCRIPT,
            _ => continue,
        };
        let bytes: Vec<u8> = match (platform, encoding) {
            (0 | 3, _) => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            (1, 0) => text.as_bytes().to_vec(),
            _ => continue,
        };
        let offset = u16::try_from(base + extra.len()).ok()?;
        u16::try_from(base + extra.len() + bytes.len()).ok()?;
        out.get_mut(r + 8..r + 12)?.copy_from_slice(&[(bytes.len() as u16).to_be_bytes(), offset.to_be_bytes()].concat());
        extra.extend(bytes);
    }
    out.extend(extra);
    Some(out)
}

fn checksum(padded: &[u8]) -> u32 {
    padded.as_chunks::<4>().0.iter().fold(0u32, |sum, w| sum.wrapping_add(u32::from_be_bytes(*w)))
}

/// Byte ranges of the sequences GPUI would keep on the app's face, though they are emoji: a keycap
/// (digit, `#` or `*`, optional U+FE0F, U+20E3) and a text-default symbol followed by U+FE0F, with
/// any ZWJ continuation. Plain symbols without U+FE0F stay text.
fn spans(text: &str) -> Vec<Range<usize>> {
    const VS: char = '\u{FE0F}';
    const ZWJ: char = '\u{200D}';
    let tone = |c: char| ('\u{1F3FB}'..='\u{1F3FF}').contains(&c);
    let mut out = Vec::new();
    let mut it = text.char_indices().peekable();
    while let Some((start, c)) = it.next() {
        let mut end = start + c.len_utf8();
        if c.is_ascii_digit() || c == '#' || c == '*' {
            let mut ahead = it.clone();
            if matches!(ahead.peek(), Some(&(_, VS))) {
                ahead.next();
            }
            if let Some(&(i, '\u{20E3}')) = ahead.peek() {
                end = i + '\u{20E3}'.len_utf8();
                it = ahead;
                it.next();
                out.push(start..end);
            }
            continue;
        }
        if c.is_ascii() || it.peek().map(|&(_, n)| n) != Some(VS) {
            continue;
        }
        end += VS.len_utf8();
        it.next();
        while let Some(&(i, n)) = it.peek() {
            if tone(n) {
                end = i + n.len_utf8();
                it.next();
            } else if n == ZWJ {
                let mut ahead = it.clone();
                ahead.next();
                let Some((j, m)) = ahead.next() else { break };
                end = j + m.len_utf8();
                if let Some(&(k, VS)) = ahead.peek() {
                    end = k + VS.len_utf8();
                    ahead.next();
                }
                it = ahead;
            } else {
                break;
            }
        }
        out.push(start..end);
    }
    out
}

/// `text` with `marks` (sorted, apart) as `StyledText`, and, once the user's own emoji font is in,
/// each emoji sequence of `spans` set in its family (requesting the family of the bundled or
/// system face would drop that face, which lacks an 'm', so nothing is set without ours); marks are cut at their
/// edges so a link or mention keeps its colour around them.
pub(crate) fn styled(text: impl Into<gpui_kit::SharedString>, marks: Vec<(Range<usize>, gpui_kit::HighlightStyle)>) -> gpui_kit::StyledText {
    let text = text.into();
    let found = if CHAIN.get().is_some() { spans(&text) } else { Vec::new() };
    if found.is_empty() {
        return gpui_kit::StyledText::new(text).with_highlights(marks);
    }
    let mut out = Vec::new();
    for (range, style) in marks {
        let mut at = range.start;
        for s in &found {
            for cut in [s.start, s.end] {
                if cut > at && cut < range.end {
                    out.push((at..cut, style));
                    at = cut;
                }
            }
        }
        out.push((at..range.end, style));
    }
    let marked = out.clone();
    for s in &found {
        let mut at = s.start;
        for (r, _) in marked.iter().filter(|(r, _)| r.start < s.end && r.end > s.start) {
            if r.start > at {
                out.push((at..r.start, Default::default()));
            }
            at = r.end;
        }
        if at < s.end {
            out.push((at..s.end, Default::default()));
        }
    }
    out.sort_by_key(|(r, _)| r.start);
    gpui_kit::StyledText::new(text).with_highlights(out).with_font_family_overrides(found.into_iter().map(|r| (r, FAMILY.into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("beeper-gpui-emoji-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    #[test]
    fn imports_and_renames_a_font() {
        let dir = dir("ttf");
        let family = import(&Path::new(FIXTURES).join("emoji-sbix.ttf"), &dir).unwrap();
        assert_eq!(family, "Test Squares Emoji");
        assert!(!dir.join(PART).exists());
        assert!(load(&dir).is_some());
        let kept = map(&path(&dir)).unwrap();
        assert_eq!(faces(Source::Binary(kept.clone())), [(FAMILY.to_owned(), POSTSCRIPT.to_owned())]);
        let face = ttf_parser::Face::parse(&kept, 0).unwrap();
        let glyph = |c| face.glyph_index(c).map(|g| g.0);
        assert_eq!(glyph('m'), glyph(' '));
        assert!(['a', '1', '\u{1F600}', '\u{2764}', '\u{1F1F5}'].into_iter().all(|c| glyph(c).is_some_and(|g| g > 3)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_bundled_font_is_ready_to_be_requested() {
        let bundled = concat!(env!("CARGO_MANIFEST_DIR"), "/android/app/src/main/assets/fonts/NotoColorEmoji.ttf");
        assert!(usable(&map(Path::new(bundled)).unwrap()));
    }

    #[test]
    #[ignore = "regenerates the bundled asset, see assets/fonts/README"]
    fn make_bundled_font() {
        let out = std::env::var("MAKE_BUNDLED_OUT").unwrap();
        import(Path::new(&std::env::var("MAKE_BUNDLED_SRC").unwrap()), Path::new(&out)).unwrap();
    }

    #[test]
    fn keeps_the_first_face_of_a_collection() {
        let dir = dir("ttc");
        assert_eq!(import(&Path::new(FIXTURES).join("emoji-sbix.ttc"), &dir).unwrap(), "Test Squares Emoji");
        assert_eq!(faces(Source::Binary(map(&path(&dir)).unwrap())).len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_new_choice_replaces_the_old() {
        let dir = dir("replace");
        import(&Path::new(FIXTURES).join("emoji-sbix.ttf"), &dir).unwrap();
        let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("android/app/src/main/assets/fonts/NotoColorEmoji.ttf");
        assert_eq!(import(&bundled, &dir).unwrap(), "Noto Color Emoji");
        assert!(load(&dir).is_some());
        remove(&dir);
        assert!(load(&dir).is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_what_it_cannot_draw() {
        let dir = dir("refuse");
        let inter = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/Inter-Regular.ttf");
        assert_eq!(import(&inter, &dir).unwrap_err(), "Not a colour emoji font.");
        let text = dir.join("notes.txt");
        fs::write(&text, "not a font").unwrap();
        assert_eq!(import(&text, &dir).unwrap_err(), "Not a font file.");
        let huge = dir.join("huge.ttf");
        File::create(&huge).unwrap().set_len(LIMIT + 1).unwrap();
        assert_eq!(import(&huge, &dir).unwrap_err(), "The file is larger than 300 MB.");
        assert!(!path(&dir).exists() && !dir.join(PART).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_broken_kept_file_is_not_loaded() {
        let dir = dir("broken");
        fs::write(path(&dir), b"garbage").unwrap();
        assert!(load(&dir).is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn emoji_spans() {
        let cases: [(&str, &[&str]); 11] = [
            ("1\u{FE0F}\u{20E3}", &["1\u{FE0F}\u{20E3}"]),
            ("a #\u{20E3} b *\u{FE0F}\u{20E3}", &["#\u{20E3}", "*\u{FE0F}\u{20E3}"]),
            ("1\u{FE0F}", &[]),
            ("123 abc", &[]),
            ("\u{2764}\u{FE0F}", &["\u{2764}\u{FE0F}"]),
            ("\u{2764} x", &[]),
            ("\u{1F1F5}\u{1F1F1} \u{1F44D}\u{1F3FD}", &[]),
            ("zażółć \u{263A}\u{FE0F}!", &["\u{263A}\u{FE0F}"]),
            ("\u{2764}\u{FE0F}\u{200D}\u{1F525}", &["\u{2764}\u{FE0F}\u{200D}\u{1F525}"]),
            ("\u{270C}\u{FE0F}\u{1F3FD}", &["\u{270C}\u{FE0F}\u{1F3FD}"]),
            ("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", &[]),
        ];
        for (text, want) in cases {
            let got: Vec<&str> = super::spans(text).into_iter().map(|r| &text[r]).collect();
            assert_eq!(got, want, "{text:?}");
        }
    }
}
