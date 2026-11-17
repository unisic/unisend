//! A colour emoji font of the user's own in place of the bundled or system one, where GPUI draws
//! text itself (Linux and Android; macOS and iOS shape with CoreText, which uses the system's).
//!
//! GPUI's text system draws a glyph in colour only from a face whose PostScript name is
//! "NotoColorEmoji", cosmic-text's own fallback looks for the family "Noto Color Emoji" on Linux,
//! and GPUI drops a face without a Latin 'm' when its family is requested. So the copy kept here
//! is renamed to those names and given an 'm', which lets it also be the root text style's
//! fallback family (see `fallbacks`).
//!
//! When that face is not Noto (the user's, or Samsung's on its phones), Noto is kept behind it
//! under a family of its own (`NOTO_FAMILY`), and each emoji the face does not draw as one glyph
//! is set in Noto instead (see `styled` and `drawable`).

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use fontdb::{Database, Language, Source};

use crate::Settings;
use crate::i18n::t;

const FILE: &str = "emoji-font";
const PART: &str = "emoji-font.part";
const LIMIT: u64 = 300 * 1024 * 1024;
const FAMILY: &str = "Noto Color Emoji";
const POSTSCRIPT: &str = "NotoColorEmoji";
const NOTO_FAMILY: &str = "Noto Color Emoji Fallback";
const SYSTEM_NOTO: &str = "/system/fonts/NotoColorEmoji.ttf";
static BUNDLED: &[u8] = include_bytes!("../assets/fonts/NotoColorEmoji.ttf");

/// An emoji font Settings offers to download: a release pinned by its SHA-256 and size, in a
/// format GPUI draws (CBDT or COLRv0) with a full-Unicode `cmap`.
pub(crate) struct Set {
    pub name: &'static str,
    /// Who made it and under what licence.
    pub credit: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// Apple's is an unofficial CBDT build: Apple licenses its emoji for no other system.
pub(crate) const SETS: [Set; 4] = [
    Set {
        name: "Apple",
        credit: "© Apple Inc.",
        url: "https://github.com/samuelngs/apple-emoji-ttf/releases/download/macos-26-20260722-484daf4e/AppleColorEmoji-Linux.ttf",
        sha256: "e37c7af6265ac4a0af6d57bc65e86109a776d9966e8343334557f63da482516f",
        size: 115_969_256,
    },
    Set {
        name: "Twemoji",
        credit: "Twitter, CC BY 4.0",
        url: "https://github.com/mozilla/twemoji-colr/releases/download/v0.7.0/Twemoji.Mozilla.ttf",
        sha256: "6d90152ee0d29e82fe2a87793af5aa4b7ad13e6538360889e141e81ed299ee8e",
        size: 1_474_284,
    },
    Set {
        name: "OpenMoji",
        credit: "OpenMoji, CC BY-SA 4.0",
        url: "https://raw.githubusercontent.com/hfg-gmuend/openmoji/17.0.0/font/OpenMoji-color-cbdt/OpenMoji-color-cbdt.ttf",
        sha256: "a541ac344a05bf1aa12585790c86bb2a40649940866d317d492f944bbf139795",
        size: 6_696_312,
    },
    Set {
        name: "Blobmoji",
        credit: "Google, Apache 2.0",
        url: "https://github.com/C1710/blobmoji/releases/download/v15.0/Blobmoji.ttf",
        sha256: "dcc3d6675036ba9b35ef574d2221532770b5bd9886cba23c645ef46947eb78f9",
        size: 12_877_044,
    },
];

/// How the download of a `Set` goes.
pub(crate) enum Fetch {
    Progress(u8),
    /// It is at `part`, for `finish`.
    Done,
    Failed(String),
}

/// Where a downloaded font is written before `finish` takes it.
pub(crate) fn part(dir: &Path) -> PathBuf {
    dir.join(PART)
}

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
    take_family(bytes, Some(dir), cx)
}

/// Adds the device's own colour emoji font on Android when GPUI can draw it (see `system`), else
/// the bundled copy (already named and given an 'm', see `assets/fonts/README`) and makes it
/// the emoji family, for systems whose own emoji font is COLRv1, which GPUI cannot draw (Fedora,
/// Android 13+). Where Android's NotoColorEmoji is CBDT, gpui-mobile has loaded it and it stays.
/// Elsewhere the system draws its own emoji (Apple's on macOS and iOS), which must stay first.
pub(crate) fn use_bundled(dir: &Path, cx: &mut gpui_kit::App) {
    if !SUPPORTED {
        return;
    }
    // Android's font directory; elsewhere it does not exist and nothing is found.
    if let Some(bytes) = system(Path::new("/system/fonts"), dir) {
        take_family(bytes, Some(dir), cx);
        return;
    }
    #[cfg(target_os = "android")]
    if has_bitmaps(Path::new(SYSTEM_NOTO)) {
        // Android keeps the flags in a font of their own, next to it in the system's fallback.
        let active = [SYSTEM_NOTO, "/system/fonts/NotoColorEmojiFlags.ttf"].into_iter().filter_map(|f| leak(map(Path::new(f))?)).collect();
        let _ = FACES.set(Faces { active, noto: None });
        return;
    }
    take_family(BUNDLED, None, cx);
}

/// Makes `bytes`, a face named FAMILY with an 'm', the only face of that family and the fallback
/// chain. Added first: requesting the family then drops the other faces of it (none has an 'm')
/// and still finds ours, where requesting a family with no face left would panic in GPUI.
/// With `dir`, the face is not Noto, which is then prepared behind it (see `noto`).
/// shortcut: leans on that dropping, which removes the system's face; replace once GPUI can draw COLRv1.
fn take_family(bytes: &'static [u8], dir: Option<&Path>, cx: &mut gpui_kit::App) -> bool {
    if let Err(e) = cx.text_system().add_fonts(vec![bytes.into()]) {
        log::warn!("emoji font: {e}");
        return false;
    }
    let _ = cx.text_system().resolve_font(&gpui_kit::font(FAMILY));
    let noto = dir.and_then(|dir| noto(Path::new(SYSTEM_NOTO), dir, bytes)).filter(|noto| {
        let added = cx.text_system().add_fonts(vec![(*noto).into()]);
        added.map_err(|e| log::warn!("emoji font: Noto: {e}")).is_ok()
    });
    let families = std::iter::once(FAMILY).chain(noto.map(|_| NOTO_FAMILY)).map(Into::into).collect();
    let _ = CHAIN.set(gpui_kit::FontFallbacks::from_fonts(families));
    let _ = FACES.set(Faces { active: vec![bytes], noto });
    true
}

/// The emoji faces, set once at start: the one in use (with Android's flag font beside it) and,
/// when that is not Noto, Noto behind it. Unset where the system draws emoji itself (macOS, iOS),
/// which then offers every emoji.
struct Faces {
    active: Vec<&'static [u8]>,
    noto: Option<&'static [u8]>,
}

static FACES: OnceLock<Faces> = OnceLock::new();

/// What draws an emoji the face in use does not.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Draw {
    Noto,
    Neither,
}

/// Every emoji of the `emojis` list, with each skin tone.
fn universe() -> impl Iterator<Item = &'static emojis::Emoji> {
    emojis::iter().flat_map(|e| std::iter::once(e).chain(e.skin_tones().into_iter().flatten().skip(1)))
}

/// The emoji of `universe` the active faces do not draw, each with what draws it instead.
/// About 20 on Samsung's font: one shaping walk of 3,963 sequences per face, a few ms.
fn gaps(active: &[ttf_parser::Face], noto: Option<&ttf_parser::Face>) -> HashMap<&'static str, Draw> {
    let pick = |emoji: &str| match noto {
        Some(noto) if draws(noto, emoji) => Draw::Noto,
        _ => Draw::Neither,
    };
    universe().map(|e| e.as_str()).filter(|e| !active.iter().any(|face| draws(face, e))).map(|e| (e, pick(e))).collect()
}

static GAPS: OnceLock<HashMap<&'static str, Draw>> = OnceLock::new();

/// `gaps` of the faces in use, worked out on first use rather than at start.
fn gaps_in_use() -> Option<&'static HashMap<&'static str, Draw>> {
    let faces = FACES.get()?;
    Some(GAPS.get_or_init(|| {
        let parse = |bytes: &&'static [u8]| ttf_parser::Face::parse(bytes, 0).ok();
        gaps(&faces.active.iter().filter_map(parse).collect::<Vec<_>>(), faces.noto.as_ref().and_then(parse).as_ref())
    }))
}

/// How a shaper treats a character when matching ligatures: selectors may stay behind unmatched,
/// joiners and tags may be skipped inside a ligature but left over mean it was not joined.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    Base,
    Selector,
    Joiner,
}

/// Whether `face` draws `emoji` as one glyph: each character mapped and a sequence (ZWJ, skin
/// tone, flag, keycap, tag) joined by the face's ligatures, applied in lookup order as a shaper
/// does. Agrees with harfrust on all 3,963 emoji for Samsung's font and the bundled Noto, but for
/// one flag Noto draws as its placeholder.
/// shortcut: only ligature lookups of every feature are applied; read contextual ones if a font builds sequences with them.
fn draws(face: &ttf_parser::Face, emoji: &str) -> bool {
    use ttf_parser::{GlyphId, gsub::SubstitutionSubtable};
    let part = |c| match c {
        '\u{FE0E}' | '\u{FE0F}' => Part::Selector,
        '\u{200D}' | '\u{E0020}'..='\u{E007F}' => Part::Joiner,
        _ => Part::Base,
    };
    let mut run: Vec<(u16, Part)> = emoji.chars().map(|c| (face.glyph_index(c).map_or(0, |g| g.0), part(c))).collect();
    for lookup in face.tables().gsub.into_iter().flat_map(|gsub| gsub.lookups) {
        for sub in lookup.subtables.into_iter::<SubstitutionSubtable>() {
            let SubstitutionSubtable::Ligature(lig) = sub else { continue };
            let mut i = 0;
            while i < run.len() {
                let set = (run[i].1 == Part::Base).then(|| lig.coverage.get(GlyphId(run[i].0))).flatten().and_then(|ix| lig.ligature_sets.get(ix));
                let joined = set.into_iter().flatten().find_map(|l| {
                    let mut j = i + 1;
                    for want in l.components {
                        loop {
                            let &(glyph, part) = run.get(j)?;
                            j += 1;
                            if glyph == want.0 {
                                break;
                            }
                            if part == Part::Base {
                                return None;
                            }
                        }
                    }
                    Some((j, l.glyph.0))
                });
                if let Some((end, glyph)) = joined {
                    run.splice(i..end, [(glyph, Part::Base)]);
                }
                i += 1;
            }
        }
    }
    let mut bases = run.iter().filter(|(_, p)| *p == Part::Base);
    matches!((bases.next(), bases.next()), (Some(&(g, _)), None) if g != 0) && run.iter().all(|(_, p)| *p != Part::Joiner)
}

/// Whether the face in use or Noto behind it draws `emoji`, one of the `emojis` list as written
/// there; a sequence outside it counts as drawable.
pub(crate) fn drawable(emoji: &str) -> bool {
    gaps_in_use().is_none_or(|gaps| gaps.get(emoji) != Some(&Draw::Neither))
}

const SYSTEM_PREFIX: &str = "emoji-system-v1-";

/// Whether the font's table directory has bitmap colour glyphs GPUI can draw (CBDT with CBLC, or
/// sbix). Reads only the directory, as the fonts are tens of MB.
fn has_bitmaps(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else { return false };
    let Ok(head) = read_at(&mut file, 0, 12) else { return false };
    let count = usize::from(be16(&head, 4).unwrap_or(0));
    let Ok(records) = read_at(&mut file, 12, 16 * count) else { return false };
    let has = |tag: &[u8]| records.chunks(16).any(|r| r.starts_with(tag));
    has(b"sbix") || (has(b"CBDT") && has(b"CBLC"))
}

/// The device's colour emoji fonts in `fonts`, best first: Samsung's, then any other `*Emoji*`
/// font with bitmaps. NotoColorEmoji is left to gpui-mobile, flag-only subsets are not emoji fonts.
fn candidates(fonts: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(fonts)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.contains("Emoji") && !name.contains("Flags") && name != "NotoColorEmoji.ttf" && (name.ends_with(".ttf") || name.ends_with(".otf"))
        })
        .collect();
    found.sort_by_key(|p| (p.file_name().is_none_or(|n| n != "SamsungColorEmoji.ttf"), p.clone()));
    found.into_iter().filter(|p| has_bitmaps(p)).collect()
}

/// The device's own emoji font, prepared once into `dir` (not the user's slot) and mapped. The
/// first start pays for copying the source (about 20 MB on Samsung); later ones only map it.
fn system(fonts: &Path, dir: &Path) -> Option<&'static [u8]> {
    let src = candidates(fonts).into_iter().next()?;
    let kept = kept(dir, SYSTEM_PREFIX, &stamp(&src)?, |part| {
        fs::copy(&src, part).map_err(|e| format!("system copy of {}: {e}", src.display()))?;
        prepare(part, FAMILY).map(drop)
    })?;
    load_file(&kept, FAMILY)
}

const NOTO_PREFIX: &str = "emoji-noto-v1-";

/// Noto, for the emoji the face in use (`active`) lacks: the device's copy at `system` where GPUI
/// can draw it, else the bundled one. Prepared once into `dir` under `NOTO_FAMILY` and with the
/// line metrics of `active` (see `fit`), so an emoji drawn from it neither moves the baseline nor
/// changes the line's height; redone when either font changes. About 10 MB on disk, written on
/// the first start; mapped, so only the pages of the glyphs drawn are read.
fn noto(system: &Path, dir: &Path, active: &[u8]) -> Option<&'static [u8]> {
    let metrics = line_metrics(&ttf_parser::Face::parse(active, 0).ok()?);
    let device = has_bitmaps(system);
    let source = if device { stamp(system)? } else { format!("bundled-{}", BUNDLED.len()) };
    let [em, ascent, descent, gap] = metrics;
    let kept = kept(dir, NOTO_PREFIX, &format!("{source}-{em}_{ascent}_{descent}_{gap}"), |part| {
        let copied = if device { fs::copy(system, part).map(drop) } else { fs::write(part, BUNDLED) };
        copied.map_err(|e| format!("Noto copy: {e}"))?;
        fit(part, metrics)?;
        prepare(part, NOTO_FAMILY).map(drop)
    })?;
    load_file(&kept, NOTO_FAMILY)
}

/// A name for `src` from its size and modification time, which change when the system's font does.
fn stamp(src: &Path) -> Option<String> {
    let meta = fs::metadata(src).ok()?;
    let age = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{}-{}.{}", meta.len(), age.as_secs(), age.subsec_nanos()))
}

/// `dir`/`prefix``stamp`, made by `make` under a part name and renamed in unless it is there
/// already, and then the only file of `prefix` kept.
fn kept(dir: &Path, prefix: &str, stamp: &str, make: impl FnOnce(&Path) -> Result<(), String>) -> Option<PathBuf> {
    let kept = dir.join(format!("{prefix}{stamp}"));
    if !kept.exists() {
        let part = dir.join(format!("{prefix}part"));
        if let Err(e) = make(&part).and_then(|()| fs::rename(&part, &kept).map_err(|e| e.to_string())) {
            log::warn!("emoji font: {e}");
            let _ = fs::remove_file(&part);
            return None;
        }
    }
    for old in fs::read_dir(dir).into_iter().flatten().flatten() {
        if old.path() != kept && old.file_name().to_str().is_some_and(|n| n.starts_with(prefix)) {
            let _ = fs::remove_file(old.path());
        }
    }
    Some(kept)
}

/// Units per em, ascent, descent and line gap, read as swash (cosmic-text's) reads them: OS/2's
/// typographic values where the font says to use them, else `hhea`'s.
fn line_metrics(face: &ttf_parser::Face) -> [i32; 4] {
    let hhea = face.tables().hhea;
    let (ascent, descent, gap) = match face.tables().os2.filter(|os2| os2.use_typographic_metrics() && os2.typographic_ascender() != 0) {
        Some(os2) => (os2.typographic_ascender(), os2.typographic_descender(), os2.typographic_line_gap()),
        None => (hhea.ascender, hhea.descender, hhea.line_gap),
    };
    [face.units_per_em().into(), ascent.into(), descent.into(), gap.into()]
}

/// Writes `metrics` (see `line_metrics`), scaled to the font's own em, over both of the font's sets
/// of line metrics, so the text system reads them whichever set it picks.
fn fit(path: &Path, [em, ascent, descent, gap]: [i32; 4]) -> Result<(), String> {
    let not_font = || "Noto copy: not a font".to_owned();
    let mut file = OpenOptions::new().read(true).write(true).open(path).map_err(|e| e.to_string())?;
    let count = be16(&read_at(&mut file, 0, 12).map_err(|e| e.to_string())?, 4).ok_or_else(not_font)?;
    let records = read_at(&mut file, 12, 16 * usize::from(count)).map_err(|e| e.to_string())?;
    let table = |tag: &[u8]| records.chunks(16).find(|r| r.starts_with(tag)).and_then(|r| Some((u64::from(be32(r, 8)?), be32(r, 12)?)));
    let (head, _) = table(b"head").ok_or_else(not_font)?;
    let own = be16(&read_at(&mut file, head + 18, 2).map_err(|e| e.to_string())?, 0).ok_or_else(not_font)?;
    let scaled: Vec<u8> = [ascent, descent, gap].iter().flat_map(|&v| ((f64::from(v) * f64::from(own) / f64::from(em.max(1))).round() as i16).to_be_bytes()).collect();
    let (hhea, _) = table(b"hhea").ok_or_else(not_font)?;
    write_at(&mut file, hhea + 4, &scaled).map_err(|e| e.to_string())?;
    if let Some((os2, _)) = table(b"OS/2").filter(|&(_, len)| len >= 74) {
        write_at(&mut file, os2 + 68, &scaled).map_err(|e| e.to_string())?;
    }
    Ok(())
}

static CHAIN: OnceLock<gpui_kit::FontFallbacks> = OnceLock::new();

/// For the root text style, once an emoji face with an 'm' is in (the user's or the bundled
/// one): it then draws every grapheme whose first character the app's face lacks (and Noto behind
/// it those whose first character it lacks too), before cosmic-text's fallback list, whose text fonts
/// come first (on Linux U+263A U+FE0F is otherwise a white glyph from Noto Sans Symbols). GPUI
/// keeps a grapheme on the app's face when it has the first character: keycaps (ASCII) and, where
/// the system's Inter 4 is the face picked, U+2764.
pub(crate) fn fallbacks() -> Option<gpui_kit::FontFallbacks> {
    CHAIN.get().cloned()
}

/// The kept font, mapped for the life of the process: only the pages of glyphs drawn are read.
fn load(dir: &Path) -> Option<&'static [u8]> {
    load_file(&path(dir), FAMILY)
}

fn load_file(path: &Path, family: &str) -> Option<&'static [u8]> {
    leak(map(path).filter(|map| usable(map, family))?)
}

fn leak(map: Arc<memmap2::Mmap>) -> Option<&'static [u8]> {
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

/// Whether every face has the names (`family`) and the 'm' that `rename` gives: without the 'm',
/// requesting the family would drop the face.
fn usable(map: &Arc<memmap2::Mmap>, family: &str) -> bool {
    let faces = faces(Source::Binary(map.clone()));
    let m = ttf_parser::Face::parse(map, 0).is_ok_and(|face| face.glyph_index('m').is_some());
    m && !faces.is_empty() && faces.iter().all(|(name, ps)| name == family && ps == POSTSCRIPT)
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
    let size = fs::metadata(src).map_err(|_| t!("The file cannot be read.").to_owned())?.len();
    if size > LIMIT {
        return Err(t!("The file is larger than 300 MB.").into());
    }
    let part = dir.join(PART);
    if let Err(e) = place(src, &part) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    finish(dir)
}

/// Makes the font at `part(dir)` the chosen one, as `import` does with a file of the user's.
pub(crate) fn finish(dir: &Path) -> Result<String, String> {
    let part = dir.join(PART);
    let made = prepare(&part, FAMILY);
    let kept = made.and_then(|family| fs::rename(&part, path(dir)).map(|()| family).map_err(|_| t!("The font cannot be saved.").to_owned()));
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
    fs::copy(src, part).map(drop).map_err(|_| t!("The file cannot be read.").to_owned())
}

/// Renames the font at `part` to `family` (see `rename`) and returns its family name before.
fn prepare(part: &Path, family: &str) -> Result<String, String> {
    let not_font = || t!("Not a font file.").to_owned();
    let was = map(part).map_or_else(Vec::new, |map| faces(Source::Binary(map))).into_iter().next().map(|(was, _)| was).ok_or_else(not_font)?;
    let mut file = OpenOptions::new().read(true).write(true).open(part).map_err(|_| not_font())?;
    rename(&mut file, family)?;
    file.sync_all().map_err(|_| t!("The font cannot be saved.").to_owned())?;
    if !map(part).is_some_and(|map| usable(&map, family)) {
        return Err(not_font());
    }
    Ok(was)
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

/// Gives the font the names the text system knows (`family` and the PostScript name in the module
/// comment) and a Latin 'm', so it can be a requested fallback family: new `name` and `cmap`
/// tables are appended and the face's table directory pointed at them. A collection keeps its
/// first face.
fn rename(file: &mut File, family: &str) -> Result<(), String> {
    let not_font = || t!("Not a font file.").to_owned();
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
            t!("This font's colour format (COLRv1) cannot be drawn yet. Choose one with bitmap emoji.").into()
        } else {
            t!("Not a colour emoji font.").into()
        });
    }
    let (names, cmap) = (table(b"name").ok_or_else(not_font)?, table(b"cmap").ok_or_else(not_font)?);
    let renamed = renamed(&read_table(names)?, family).ok_or_else(not_font)?;
    let with_m = with_m(&read_table(cmap)?).ok_or_else(|| t!("Not a colour emoji font.").to_owned())?;
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
fn renamed(table: &[u8], family: &str) -> Option<Vec<u8>> {
    let count = usize::from(be16(table, 2)?);
    let storage = usize::from(be16(table, 4)?);
    let base = table.len().checked_sub(storage)?;
    let mut out = table.to_vec();
    let mut extra = Vec::new();
    for i in 0..count {
        let r = 6 + 12 * i;
        let (platform, encoding, id) = (be16(table, r)?, be16(table, r + 2)?, be16(table, r + 6)?);
        let text = match id {
            1 | 4 | 16 | 21 => family,
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

/// Characters in the longest emoji of the `emojis` list (a kiss with two skin tones).
const LONGEST: usize = 10;

/// Byte ranges of the emoji in `text` that `gaps` sets in Noto, each the longest of the `emojis`
/// list at its place. A text-default symbol without U+FE0F stays text, as in `spans`.
fn noto_spans(text: &str, gaps: &HashMap<&'static str, Draw>) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if !gaps.values().any(|d| *d == Draw::Noto) {
        return out;
    }
    let mut at = 0;
    while let Some(c) = text[at..].chars().next() {
        let next = text[at + c.len_utf8()..].chars().next();
        let keycap = matches!(c, '#' | '*' | '0'..='9') && matches!(next, Some('\u{FE0F}' | '\u{20E3}'));
        let starts = keycap || matches!(c, '\u{A9}' | '\u{AE}' | '\u{2000}'..='\u{2BFF}' | '\u{3030}' | '\u{303D}' | '\u{3297}' | '\u{3299}' | '\u{1F000}'..);
        let ends: Vec<usize> = if starts { text[at..].char_indices().map(|(i, c)| at + i + c.len_utf8()).take(LONGEST).collect() } else { Vec::new() };
        let Some((end, emoji)) = ends.into_iter().rev().find_map(|end| Some((end, emojis::get(&text[at..end])?))) else {
            at += c.len_utf8();
            continue;
        };
        let written = &text[at..end];
        let plain = written.chars().count() == 1 && emoji.as_str().contains('\u{FE0F}') && !written.contains('\u{FE0F}');
        if !plain && gaps.get(emoji.as_str()) == Some(&Draw::Noto) {
            out.push(at..end);
        }
        at = end;
    }
    out
}

/// The sequences of `text` set in an emoji family, by start: each of `spans` in the face in use,
/// and each of `noto_spans` in Noto. Empty without `CHAIN`: the face by that name then lacks an
/// 'm' and requesting the family would drop it.
fn families(text: &str) -> Vec<(Range<usize>, &'static str)> {
    if CHAIN.get().is_none() {
        return Vec::new();
    }
    let noto = gaps_in_use().map_or_else(Vec::new, |gaps| noto_spans(text, gaps));
    let apart = |s: &Range<usize>| !noto.iter().any(|n| n.start < s.end && s.start < n.end);
    let mut found: Vec<_> = spans(text).into_iter().filter(apart).map(|s| (s, FAMILY)).collect();
    found.extend(noto.into_iter().map(|n| (n, NOTO_FAMILY)));
    found.sort_by_key(|(r, _)| r.start);
    found
}

/// `text` with `marks` (sorted, apart) as `StyledText`, and each sequence of `families` set in its
/// family. Marks are cut at the sequences' edges so a link or mention keeps its colour around them.
pub(crate) fn styled(text: impl Into<gpui_kit::SharedString>, marks: Vec<(Range<usize>, gpui_kit::HighlightStyle)>) -> gpui_kit::StyledText {
    let text = text.into();
    let found = families(&text);
    if found.is_empty() {
        return gpui_kit::StyledText::new(text).with_highlights(marks);
    }
    let mut out = Vec::new();
    for (range, style) in marks {
        let mut at = range.start;
        for (s, _) in &found {
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
    for (s, _) in &found {
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
    gpui_kit::StyledText::new(text).with_highlights(out).with_font_family_overrides(found.into_iter().map(|(r, family)| (r, family.into())))
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

    fn stamped(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir).unwrap().flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.starts_with(SYSTEM_PREFIX)).collect();
        names.sort();
        names
    }

    #[test]
    fn prepares_the_device_font_once_and_again_when_it_changes() {
        let (fonts, data) = (dir("sys-fonts"), dir("sys-data"));
        let sbix = Path::new(FIXTURES).join("emoji-sbix.ttf");
        let inter = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/Inter-Regular.ttf");
        fs::copy(&inter, fonts.join("RobotoEmoji.ttf")).unwrap();
        fs::copy(&inter, fonts.join("NotoColorEmojiFlags.ttf")).unwrap();
        assert!(system(&fonts, &data).is_none() && stamped(&data).is_empty());
        let sam = fonts.join("SamsungColorEmoji.ttf");
        fs::copy(&sbix, &sam).unwrap();
        assert_eq!(candidates(&fonts), std::slice::from_ref(&sam));
        assert!(system(&fonts, &data).is_some());
        let first = stamped(&data);
        assert_eq!(first.len(), 1);
        assert!(usable(&map(&data.join(&first[0])).unwrap(), FAMILY));
        assert!(system(&fonts, &data).is_some());
        assert_eq!(stamped(&data), first);
        fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/NotoColorEmoji.ttf"), &sam).unwrap();
        assert!(system(&fonts, &data).is_some());
        let second = stamped(&data);
        assert!(second.len() == 1 && second != first);
        assert!(!data.join(format!("{SYSTEM_PREFIX}part")).exists());
        fs::remove_dir_all(fonts).unwrap();
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn prepares_the_real_samsung_font_when_present() {
        let src = Path::new("/tmp/samsung-emoji/SamsungColorEmoji.ttf");
        if !src.exists() {
            return;
        }
        let (fonts, data) = (dir("sam-fonts"), dir("sam-data"));
        fs::copy(src, fonts.join("SamsungColorEmoji.ttf")).unwrap();
        let t = std::time::Instant::now();
        assert!(system(&fonts, &data).is_some());
        let first = t.elapsed();
        let t = std::time::Instant::now();
        assert!(system(&fonts, &data).is_some());
        eprintln!("samsung first start {first:?}, later start {:?}", t.elapsed());
        assert_eq!(stamped(&data).len(), 1);
        fs::remove_dir_all(fonts).unwrap();
        fs::remove_dir_all(data).unwrap();
    }

    /// Each set offered for download, when a copy fetched by hand is in /tmp/emoji-sets.
    #[test]
    fn the_downloadable_sets_are_taken_when_present() {
        for set in &SETS {
            let src = Path::new("/tmp/emoji-sets").join(set.url.rsplit('/').next().unwrap());
            let Ok(bytes) = fs::read(&src) else { continue };
            let sum: String = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &bytes).as_ref().iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!((bytes.len() as u64, sum.as_str()), (set.size, set.sha256), "{}", set.name);
            let data = dir(&format!("set-{}", set.name));
            fs::write(part(&data), &bytes).unwrap();
            let family = finish(&data).unwrap_or_else(|e| panic!("{}: {e}", set.name));
            eprintln!("{}: {family}", set.name);
            assert!(load(&data).is_some() && !part(&data).exists());
            fs::remove_dir_all(data).unwrap();
        }
    }

    #[test]
    fn the_bundled_font_is_ready_to_be_requested() {
        let bundled = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/NotoColorEmoji.ttf");
        assert!(usable(&map(Path::new(bundled)).unwrap(), FAMILY));
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
        let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/NotoColorEmoji.ttf");
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
    fn draws_follows_the_face() {
        let seed = Path::new("/tmp/beeper-gpui-seed/fonts");
        let mut maps: Vec<(&str, &[u8])> = vec![("bundled", BUNDLED)];
        let found: Vec<_> = ["NotoColorEmoji.ttf", "SamsungColorEmoji.ttf"].into_iter().filter_map(|name| Some((name, map(&seed.join(name))?))).collect();
        maps.extend(found.iter().map(|(name, map)| (*name, &map[..])));
        let all = universe().count();
        for (name, bytes) in maps {
            let face = ttf_parser::Face::parse(bytes, 0).unwrap();
            let flag = |e: &&str| e.starts_with(|c| ('\u{1F1E6}'..='\u{1F1FF}').contains(&c));
            // Android's own Noto leaves the flags to NotoColorEmojiFlags.ttf.
            let missing: Vec<&str> = universe().map(|e| e.as_str()).filter(|e| !draws(&face, e) && !(name == "NotoColorEmoji.ttf" && flag(e))).collect();
            eprintln!("{name}: {} of {all} emoji missing", missing.len());
            assert!(missing.len() < all / 10, "{name}");
            for e in ["\u{1F600}", "\u{2764}\u{FE0F}", "\u{1F44B}\u{1F3FD}", "#\u{FE0F}\u{20E3}", "\u{1F1F5}\u{1F1F1}", "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}"] {
                assert!(draws(&face, e) || (name == "NotoColorEmoji.ttf" && flag(&e)), "{name}: {e:?}");
            }
        }
    }

    #[test]
    fn a_sequence_is_drawn_only_when_joined() {
        let fixture = fs::read(Path::new(FIXTURES).join("emoji-sbix.ttf")).unwrap();
        let face = ttf_parser::Face::parse(&fixture, 0).unwrap();
        // The fixture joins a family, a toned thumb, the Polish flag and keycap one.
        for e in ["\u{1F600}", "\u{2764}\u{FE0F}", "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", "\u{1F44D}\u{1F3FD}", "\u{1F1F5}\u{1F1F1}", "1\u{FE0F}\u{20E3}"] {
            assert!(draws(&face, e), "{e:?}");
        }
        // Every part is mapped, but no ligature joins them: a shaper draws them side by side.
        for e in ["\u{1F600}\u{200D}\u{1F600}", "\u{1F600}\u{1F600}", "\u{1F600}\u{1F3FD}", "\u{1F1F1}\u{1F1F5}", "\u{1F600}\u{E0067}\u{E007F}", "\u{1F466}"] {
            assert!(!draws(&face, e), "{e:?}");
        }
    }

    fn named(name: &str) -> &'static str {
        universe().find(|e| e.name() == name).unwrap().as_str()
    }

    #[test]
    fn noto_fills_what_the_face_lacks() {
        let fixture = fs::read(Path::new(FIXTURES).join("emoji-sbix.ttf")).unwrap();
        let active = [ttf_parser::Face::parse(&fixture, 0).unwrap()];
        let noto = ttf_parser::Face::parse(BUNDLED, 0).unwrap();
        let with = gaps(&active, Some(&noto));
        let without = gaps(&active, None);
        let (family, couple) = (named("family: man, woman, girl"), named("couple with heart: man, man"));
        assert_eq!((with.get("\u{1F600}"), with.get(family)), (None, None));
        // Man, heart and joiner are in the fixture, the ligature for the couple only in Noto.
        assert_eq!((with.get(couple), with.get(named("boy"))), (Some(&Draw::Noto), Some(&Draw::Noto)));
        assert_eq!(with.get(named("cracking face")), Some(&Draw::Neither));
        assert!(without.values().all(|d| *d == Draw::Neither) && without.len() == with.len());
        let shown = |gaps: &HashMap<&str, Draw>| universe().filter(|e| gaps.get(e.as_str()) != Some(&Draw::Neither)).count();
        let either = universe().filter(|e| draws(&active[0], e.as_str()) || draws(&noto, e.as_str())).count();
        assert_eq!(shown(&with), either);
        assert_eq!(shown(&without), universe().filter(|e| draws(&active[0], e.as_str())).count());
        assert!(shown(&without) < 20 && shown(&with) > 3000);
    }

    #[test]
    fn samsung_gaps_when_present() {
        let Some(samsung) = map(Path::new("/tmp/samsung-emoji/SamsungColorEmoji.ttf")) else { return };
        let active = [ttf_parser::Face::parse(&samsung, 0).unwrap()];
        let gaps = gaps(&active, Some(&ttf_parser::Face::parse(BUNDLED, 0).unwrap()));
        let neither = gaps.values().filter(|d| **d == Draw::Neither).count();
        eprintln!("samsung: {} lacking, {neither} in neither, {} of {} shown", gaps.len(), universe().count() - neither, universe().count());
        assert_eq!(gaps.get(named("eye in speech bubble")), Some(&Draw::Noto));
        assert_eq!(gaps.get(named("cracking face")), Some(&Draw::Neither));
    }

    #[test]
    fn noto_spans_take_the_whole_sequence() {
        assert_eq!(universe().map(|e| e.as_str().chars().count()).max(), Some(LONGEST));
        let eye = named("eye in speech bubble");
        let gaps = HashMap::from([(eye, Draw::Noto), ("\u{2764}\u{FE0F}", Draw::Noto), ("\u{1F600}", Draw::Neither)]);
        let text = format!("a {eye}\u{1F600} \u{2764} \u{2764}\u{FE0F}1\u{FE0F}\u{20E3}");
        let got: Vec<&str> = noto_spans(&text, &gaps).into_iter().map(|r| &text[r]).collect();
        assert_eq!(got, [eye, "\u{2764}\u{FE0F}"]);
        assert!(noto_spans(&text, &HashMap::from([("\u{1F600}", Draw::Neither)])).is_empty());
    }

    #[test]
    fn noto_is_prepared_with_the_face_line_metrics() {
        let data = dir("noto");
        let fixture = fs::read(Path::new(FIXTURES).join("emoji-sbix.ttf")).unwrap();
        let none = Path::new("/nonexistent/NotoColorEmoji.ttf");
        let em = |m: [i32; 4]| [m[1], m[2], m[3]].map(|v| f64::from(v) / f64::from(m[0]));
        let want = line_metrics(&ttf_parser::Face::parse(&fixture, 0).unwrap());
        assert_ne!(em(want), em(line_metrics(&ttf_parser::Face::parse(BUNDLED, 0).unwrap())));
        let got = line_metrics(&ttf_parser::Face::parse(noto(none, &data, &fixture).unwrap(), 0).unwrap());
        assert!(em(got).iter().zip(em(want)).all(|(g, w)| (g - w).abs() < 1e-3), "{got:?} {want:?}");
        let named = |prefix| fs::read_dir(&data).unwrap().flatten().filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(prefix))).count();
        assert_eq!(named(NOTO_PREFIX), 1);
        assert!(noto(none, &data, BUNDLED).is_some());
        assert_eq!(named(NOTO_PREFIX), 1);
        fs::remove_dir_all(data).unwrap();
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
