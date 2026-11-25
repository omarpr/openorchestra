//! An Android emulator's device frame, from its AVD's skin in the SDK
//! (`<sdk>/skins/pixel_9_pro/`): read at run time, nothing of Google's is
//! copied into the repo or the app.
//!
//! - `<avd_dir>/config.ini`: `skin.path`, the skin's directory (absolute, or
//!   a name under `<sdk>/skins/`). An AVD made with a size rather than a
//!   device's skin (`skin.path = 1080x2400`, `skin.dynamic = yes`) has none.
//! - `<skin>/layout`: a small brace format ([`parse_layout`]). The display's
//!   size and corner radius (`parts.device.display`), the frame's art
//!   (`parts.<part>.background.image` and `foreground.mask`), and the layout
//!   that places them (`layouts.portrait`: its size, and each part's
//!   offset). The Pixel 9 Pro's display is 1280 × 2856 at (60, 61) in a
//!   1408 × 2974 frame, with corners of 109.
//! - `back.webp`: the frame, drawn at its own size where its part goes.
//!   `mask.webp`: the display's size, clear where the screen shows and
//!   opaque over its corners and the camera's cutout; the emulator draws it
//!   over the screen, and so does the overlay.
//!
//! Everything is in the skin's pixels, which are the display's: the frame
//! is laid out as a simulator's is in points (`chrome::ChromeSpec`). An AVD
//! with no skin (the common case) still gets rounded corners, from its
//! `hw.lcd.width` and `hw.lcd.height` and a default radius
//! ([`DEFAULT_CORNER`]). A skin that can't be read is no frame, never a
//! panic.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::devices::chrome::{ChromeSpec, FrameArt, Insets, ScreenMask};

/// A screen with no skin, or a skin with no `corner_radius`, gets corners
/// of this much of its shorter side: about a Pixel's (the Pixel 9 Pro's
/// 109 on 1280 is 0.085).
pub(crate) const DEFAULT_CORNER: f32 = 0.08;

/// A `layout` bigger than this isn't a skin's (the Pixel 9 Pro's is 0.6 KB).
const MAX_LAYOUT: u64 = 64 * 1024;

/// How deep a `layout`'s braces may nest (real ones go four deep).
const MAX_DEPTH: usize = 16;

/// A rectangle in a skin's pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SkinRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// What a skin's `layout` says, for its upright layout.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SkinLayout {
    /// The whole layout, the frame, in the skin's pixels.
    pub size: (u32, u32),
    /// Where the display is in it, and its size.
    pub display: SkinRect,
    /// The display's corner radius, when the skin gives one.
    pub corner_radius: Option<f32>,
    /// The frame's background image (`back.webp`) and its top left.
    pub background: Option<(String, (i32, i32))>,
    /// The foreground drawn over the display (`mask.webp`).
    pub mask: Option<String>,
}

/// An AVD's skin, its art found on disk.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AndroidSkin {
    pub layout: SkinLayout,
    pub back: PathBuf,
    pub mask: Option<PathBuf>,
}

impl AndroidSkin {
    /// What the Device tab draws with this skin: its frame, shaped by the
    /// skin's corner radius and its art (by the art alone when the skin
    /// gives no radius), and frame off, its screen rounded by that radius
    /// or the default. Reads the foreground, to measure the corners it
    /// shows ([`shown_corner`]): off the UI thread.
    pub(crate) fn spec(&self) -> ChromeSpec {
        let SkinLayout { size, display: d, corner_radius, background, .. } = &self.layout;
        let (w, h) = (d.w as f32, d.h as f32);
        let back_at = background.as_ref().map_or((0, 0), |(_, at)| *at);
        let screen_corner = corner_radius.unwrap_or_else(|| default_corner((d.w, d.h)));
        // Framed, the screen is cut by the skin's radius (none without one)
        // and its foreground both. Art that can't be read: the screen's own.
        let cut = corner_radius.unwrap_or(0.0);
        let shown = match &self.mask {
            None => shown_corner(None, d.w, cut),
            Some(path) => art(path).and_then(|mask| shown_corner(Some(&mask), d.w, cut)),
        };
        ChromeSpec {
            screen_pt: Some((w, h)),
            screen_corner_pt: Some(screen_corner),
            mask: Some(ScreenMask::Rounded),
            frame: Some(FrameArt::Skin {
                back: self.back.clone(),
                back_at: (back_at.0 as f32, back_at.1 as f32),
                foreground: self.mask.clone(),
                corner: *corner_radius,
                shown_corner: shown.unwrap_or(screen_corner),
                sizing: Insets {
                    left: d.x as f32,
                    top: d.y as f32,
                    right: size.0 as f32 - d.x as f32 - w,
                    bottom: size.1 as f32 - d.y as f32 - h,
                },
            }),
        }
    }
}

/// The corner radius of a `(w, h)` screen that doesn't say.
fn default_corner((w, h): (u32, u32)) -> f32 {
    w.min(h) as f32 * DEFAULT_CORNER
}

/// The radius of the screen's corners a framed skin shows, in the display's
/// pixels (`display_w` wide): the larger of `cut`, the radius the screen is
/// cut by, and the curve the foreground `mask` (stretched over the display)
/// leaves clear. No mask: `cut`. None when the mask can't be measured.
pub(crate) fn shown_corner(mask: Option<&image::RgbaImage>, display_w: u32, cut: f32) -> Option<f32> {
    let Some(mask) = mask else {
        return Some(cut);
    };
    let (w, h) = mask.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let mut art = 0.0f32;
    for (right, bottom) in [(false, false), (true, false), (false, true), (true, true)] {
        let alpha = |k: u32| {
            let x = if right { w - 1 - k } else { k };
            let y = if bottom { h - 1 - k } else { k };
            mask.get_pixel(x, y).0[3]
        };
        art = art.max(corner_band(alpha, w.min(h))? / DIAGONAL);
    }
    Some(cut.max(art * display_w as f32 / w as f32))
}

/// How far in, across, a corner's circle meets the diagonal from the
/// corner, per pixel of its radius: 1 − 1/√2.
const DIAGONAL: f32 = 1.0 - std::f32::consts::FRAC_1_SQRT_2;

/// The alpha at and above which a foreground's pixel covers the screen.
const COVERS: u8 = 128;

/// How far in, across, a corner's foreground stops covering the screen,
/// read along its diagonal (`alpha` at each step from the corner) in art
/// `short` pixels on its shorter side: past whatever clear sliver the very
/// corner has, to where its opaque band clears again, between pixels. 0
/// when it covers nothing within a corner of half the short side; None when
/// it is still covering there.
fn corner_band(alpha: impl Fn(u32) -> u8, short: u32) -> Option<f32> {
    let limit = ((short as f32 / 2.0) * DIAGONAL) as u32;
    let mut k = 0;
    while k <= limit && alpha(k) < COVERS {
        k += 1;
    }
    if k > limit {
        return Some(0.0);
    }
    while k <= limit && alpha(k) >= COVERS {
        k += 1;
    }
    if k > limit {
        return None;
    }
    // Between the last covering pixel's centre and the first clear one's,
    // where the alpha crosses half.
    let (a, b) = (f32::from(alpha(k - 1)), f32::from(alpha(k)));
    Some(k as f32 - 0.5 + (a - f32::from(COVERS)) / (a - b))
}

/// A `layout` file read as a tree: each `key value` line, and each
/// `key { … }` block, in order.
#[derive(Debug, Default)]
struct Node {
    entries: Vec<(String, Entry)>,
}

#[derive(Debug)]
enum Entry {
    Value(String),
    Node(Node),
}

impl Node {
    fn node(&self, key: &str) -> Option<&Node> {
        self.nodes().find(|(k, _)| *k == key).map(|(_, n)| n)
    }

    /// Its blocks, in order.
    fn nodes(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.entries.iter().filter_map(|(k, e)| match e {
            Entry::Node(n) => Some((k.as_str(), n)),
            Entry::Value(_) => None,
        })
    }

    fn value(&self, key: &str) -> Option<&str> {
        self.entries.iter().find_map(|(k, e)| match e {
            Entry::Value(v) if k == key => Some(v.as_str()),
            _ => None,
        })
    }

    fn number<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.value(key)?.parse().ok()
    }

    /// A size: a whole number above 0.
    fn size(&self, key: &str) -> Option<u32> {
        self.number::<u32>(key).filter(|&v| v > 0)
    }

    /// An offset: a whole number, 0 when it isn't given.
    fn offset(&self, key: &str) -> Option<i32> {
        match self.value(key) {
            None => Some(0),
            Some(v) => v.parse().ok(),
        }
    }
}

/// The tree of a `layout`. None when its braces don't balance or nest too
/// deep. A key on a line of its own opens the block when the next line is
/// `{`; otherwise it is ignored.
fn parse_tree(text: &str) -> Option<Node> {
    let mut open: Vec<(String, Node)> = vec![(String::new(), Node::default())];
    let mut pending: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (head, opens) = match line.strip_suffix('{') {
            Some(head) => (head.trim(), true),
            None => (line, false),
        };
        if head == "}" {
            if open.len() < 2 {
                return None;
            }
            let (key, node) = open.pop()?;
            open.last_mut()?.1.entries.push((key, Entry::Node(node)));
            pending = None;
            continue;
        }
        if opens {
            let key = if head.is_empty() { pending.take()? } else { head.to_string() };
            if open.len() > MAX_DEPTH {
                return None;
            }
            open.push((key, Node::default()));
            continue;
        }
        pending = None;
        match head.split_once(char::is_whitespace) {
            Some((key, value)) => {
                let value = Entry::Value(value.trim().to_string());
                open.last_mut()?.1.entries.push((key.to_string(), value));
            }
            None => pending = Some(head.to_string()),
        }
    }
    (open.len() == 1).then(|| open.pop()).flatten().map(|(_, root)| root)
}

/// A skin's `layout`, for its upright layout. None when it has no display,
/// no layout that places it unturned, or the display isn't inside the
/// layout. Unknown keys are ignored.
pub(crate) fn parse_layout(text: &str) -> Option<SkinLayout> {
    let root = parse_tree(text)?;
    let parts = root.node("parts")?;
    let display = parts.node("device")?.node("display")?;
    let (w, h) = (display.size("width")?, display.size("height")?);
    let inner = (display.offset("x")?, display.offset("y")?);
    let corner_radius = display
        .number::<f32>("corner_radius")
        .filter(|r| r.is_finite() && *r >= 0.0);
    // Portrait first, then the rest in order: the first that places the
    // device unturned (a tablet's only one is "landscape").
    let layouts = root.node("layouts")?;
    let ordered = layouts
        .nodes()
        .filter(|(k, _)| *k == "portrait")
        .chain(layouts.nodes().filter(|(k, _)| *k != "portrait"));
    for (_, layout) in ordered {
        let Some(found) = upright(layout, parts, (w, h), inner, corner_radius) else {
            continue;
        };
        return Some(found);
    }
    None
}

/// `layout` (one of `layouts`) read, if it places the device unturned, with
/// the display inside it.
fn upright(
    layout: &Node,
    parts: &Node,
    (w, h): (u32, u32),
    inner: (i32, i32),
    corner_radius: Option<f32>,
) -> Option<SkinLayout> {
    let size = (layout.size("width")?, layout.size("height")?);
    let mut device = None;
    let mut frame = None;
    for (_, part) in layout.nodes() {
        let Some(name) = part.value("name") else {
            continue;
        };
        let at = (part.offset("x")?, part.offset("y")?);
        if name == "device" {
            if part.offset("rotation")? != 0 {
                return None;
            }
            device = Some(at);
        } else if frame.is_none() {
            frame = Some((name, at));
        }
    }
    let (dx, dy) = device?;
    let (x, y) = (dx.checked_add(inner.0)?, dy.checked_add(inner.1)?);
    let inside = x >= 0
        && y >= 0
        && i64::from(x) + i64::from(w) <= i64::from(size.0)
        && i64::from(y) + i64::from(h) <= i64::from(size.1);
    if !inside {
        return None;
    }
    let art = frame.and_then(|(name, at)| Some((parts.node(name)?, at)));
    let background = art.and_then(|(part, at)| {
        Some((part.node("background")?.value("image")?.to_string(), at))
    });
    let mask = art
        .and_then(|(part, _)| part.node("foreground")?.value("mask"))
        .map(str::to_string);
    Some(SkinLayout {
        size,
        display: SkinRect { x, y, w, h },
        corner_radius,
        background,
        mask,
    })
}

/// `key`'s value in an AVD's `config.ini`.
fn config_value<'a>(config: &'a str, key: &str) -> Option<&'a str> {
    config
        .lines()
        .filter_map(|line| line.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

/// An image name from a layout, as a file in `dir`: a plain name only.
fn art_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let plain = Path::new(name).file_name().is_some_and(|f| f == name);
    plain.then(|| dir.join(name)).filter(|p| p.is_file())
}

/// The skin `config` (an AVD's `config.ini`) names, with relative names
/// under `skins` (`<sdk>/skins`). None without one whose layout and
/// background are there.
pub(crate) fn skin_from(config: &str, skins: Option<&Path>) -> Option<AndroidSkin> {
    let named = Path::new(config_value(config, "skin.path")?);
    let dir = if named.is_absolute() { named.to_path_buf() } else { skins?.join(named) };
    let file = dir.join("layout");
    if std::fs::metadata(&file).ok()?.len() > MAX_LAYOUT {
        return None;
    }
    let layout = parse_layout(&std::fs::read_to_string(&file).ok()?)?;
    let back = art_in(&dir, &layout.background.as_ref()?.0)?;
    let mask = layout.mask.as_deref().and_then(|name| art_in(&dir, name));
    Some(AndroidSkin { layout, back, mask })
}

/// The skin of the AVD at `avd_dir`. Reads its `config.ini` and the skin's
/// `layout`: off the UI thread.
pub(crate) fn skin_for(avd_dir: &Path) -> Option<Arc<AndroidSkin>> {
    let config = std::fs::read_to_string(avd_dir.join("config.ini")).ok()?;
    let skins = super::sdk::sdk().map(|sdk| sdk.join("skins"));
    skin_from(&config, skins.as_deref()).map(Arc::new)
}

/// What the Device tab draws an AVD with (its `config.ini` is `config`):
/// its `skin`'s frame when that is for its screen's size, else just its
/// screen's rounded corners. None when even the screen's size is unknown.
pub(crate) fn spec_for_avd(config: &str, skin: Option<&AndroidSkin>) -> Option<ChromeSpec> {
    let native = super::stream::native_size(config);
    if let Some(skin) = skin {
        let d = skin.layout.display;
        // A skin for another size would stretch the picture to its own.
        if native.is_none_or(|size| size == (d.w, d.h)) {
            return Some(skin.spec());
        }
        log::debug!("device: the skin is {}×{}, the screen {native:?}: no frame", d.w, d.h);
    }
    let (w, h) = native?;
    Some(ChromeSpec {
        screen_pt: Some((w as f32, h as f32)),
        screen_corner_pt: Some(default_corner((w, h))),
        mask: Some(ScreenMask::Rounded),
        frame: None,
    })
}

/// [`spec_for_avd`] for the AVD at `avd_dir`. Reads files: off the UI
/// thread.
pub(crate) fn chrome_for_avd(avd_dir: &Path) -> Option<Arc<ChromeSpec>> {
    let config = std::fs::read_to_string(avd_dir.join("config.ini")).ok()?;
    spec_for_avd(&config, skin_for(avd_dir).as_deref()).map(Arc::new)
}

/// The overlay cache's name for the AVD at `avd_dir`.
pub(crate) fn chrome_key(avd_dir: &Path) -> String {
    format!("android:{}", avd_dir.display())
}

/// How many decoded images [`art`] keeps: one skin's background and
/// foreground.
const ART_KEPT: usize = 2;

/// Decoded art, the most recently used last, each with its file's modification time, so a
/// changed file is decoded again.
struct ArtCache {
    kept: Vec<(PathBuf, Option<SystemTime>, Arc<image::RgbaImage>)>,
}

impl ArtCache {
    const fn new() -> Self {
        Self { kept: Vec::new() }
    }

    /// The art at `path`, kept if it was decoded (`decode`) before and the
    /// file is unchanged since.
    fn get(
        &mut self,
        path: &Path,
        decode: impl FnOnce(&Path) -> Option<image::RgbaImage>,
    ) -> Option<Arc<image::RgbaImage>> {
        let modified = std::fs::metadata(path).ok()?.modified().ok();
        if let Some(at) = self.kept.iter().position(|(p, m, _)| p == path && *m == modified) {
            // Used now: the last to go.
            let entry = self.kept.remove(at);
            let art = entry.2.clone();
            self.kept.push(entry);
            return Some(art);
        }
        let art = Arc::new(decode(path)?);
        self.kept.retain(|(p, _, _)| p != path);
        self.kept.push((path.to_path_buf(), modified, art.clone()));
        if self.kept.len() > ART_KEPT {
            self.kept.remove(0);
        }
        Some(art)
    }
}

/// A skin's image, decoded once for the overlays drawn while it is shown
/// (each resize, turn or theme draws one again with the same two images).
/// Off the UI thread: the first time decodes.
pub(crate) fn art(path: &Path) -> Option<Arc<image::RgbaImage>> {
    static ART: Mutex<ArtCache> = Mutex::new(ArtCache::new());
    ART.lock().unwrap_or_else(|e| e.into_inner()).get(path, decode_art)
}

/// A skin's image (WebP), as straight RGBA. None if it can't be decoded.
pub(crate) fn decode_art(path: &Path) -> Option<image::RgbaImage> {
    match image::open(path) {
        Ok(art) => Some(art.into_rgba8()),
        Err(e) => {
            log::info!("device: couldn't read the skin's {}: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The Pixel 9 Pro's `layout` from the SDK (`skins/pixel_9_pro`),
    /// trimmed of its `event` line: numbers and names only, no artwork.
    pub(crate) const PIXEL_9_PRO: &str = include_str!("../testdata/pixel_9_pro.layout");

    /// An older skin's shape (nexus_5): no corner radius, no mask, its own
    /// image names, and a turned landscape layout besides.
    const NEXUS_5: &str = "\
parts {
  device {
    display {
      width 1080
      height 1920
      x 0
      y 0
    }
  }
  portrait {
    background {
      image port_back.webp
    }
    onion {
      image port_fore.webp
    }
  }
  landscape {
    background {
      image land_back.webp
    }
  }
}
layouts {
  landscape {
    width 2497
    height 1235
    part1 {
      name landscape
      x 0
      y 0
    }
    part2 {
      name device
      x 261
      y 1145
      rotation 3
    }
  }
  portrait {
    width 1370
    height 2405
    part1 {
      name portrait
      x 0
      y 0
    }
    part2 {
      name device
      x 144
      y 195
    }
  }
}
";

    /// What an AVD with `config` gets, its skin looked for under `skins`.
    fn spec_with(config: &str, skins: Option<&Path>) -> Option<ChromeSpec> {
        spec_for_avd(config, skin_from(config, skins).as_ref())
    }

    /// `config.ini` lines as the AVD manager writes them (` = `).
    fn config(skin_path: &str, w: u32, h: u32) -> String {
        format!(
            "avd.ini.encoding = UTF-8\nhw.lcd.density = 480\nhw.lcd.height = {h}\nhw.lcd.width = {w}\n\
             skin.dynamic = yes\nskin.name = x\nskin.path = {skin_path}\n"
        )
    }

    /// A skin dir in `dir` named `name`, with the Pixel 9 Pro's layout and
    /// stand-in art (small coloured WebPs, not Google's).
    fn install_skin(dir: &Path, name: &str) -> PathBuf {
        let skin = dir.join(name);
        std::fs::create_dir_all(&skin).unwrap();
        std::fs::write(skin.join("layout"), PIXEL_9_PRO).unwrap();
        write_webp(&skin.join("back.webp"), 1408 / 8, 2974 / 8, [200, 30, 40, 255]);
        write_webp(&skin.join("mask.webp"), 1280 / 8, 2856 / 8, [0, 0, 0, 0]);
        skin
    }

    pub(crate) fn write_webp(path: &Path, w: u32, h: u32, colour: [u8; 4]) {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(colour));
        img.save_with_format(path, image::ImageFormat::WebP).unwrap();
    }

    #[test]
    fn parses_pixel_9_pro_layout() {
        let layout = parse_layout(PIXEL_9_PRO).unwrap();
        assert_eq!(layout.size, (1408, 2974));
        assert_eq!(layout.display, SkinRect { x: 60, y: 61, w: 1280, h: 2856 });
        assert_eq!(layout.corner_radius, Some(109.0));
        assert_eq!(layout.background, Some(("back.webp".to_string(), (0, 0))));
        assert_eq!(layout.mask.as_deref(), Some("mask.webp"));
        // Its frame: 60 left, 61 top, 68 right (the buttons' side), 57 below.
        let spec = AndroidSkin { layout, back: "/s/back.webp".into(), mask: None }.spec();
        assert_eq!(spec.screen_pt, Some((1280.0, 2856.0)));
        assert_eq!(spec.screen_corner_pt, Some(109.0));
        assert_eq!(
            spec.geometry().unwrap().sizing,
            Insets { left: 60.0, top: 61.0, right: 68.0, bottom: 57.0 }
        );
    }

    #[test]
    fn an_older_skins_layout_parses_too() {
        // The unturned layout, wherever it is; its own names; no corners
        // or mask.
        let layout = parse_layout(NEXUS_5).unwrap();
        assert_eq!(layout.size, (1370, 2405));
        assert_eq!(layout.display, SkinRect { x: 144, y: 195, w: 1080, h: 1920 });
        assert_eq!(layout.corner_radius, None);
        assert_eq!(layout.background, Some(("port_back.webp".to_string(), (0, 0))));
        assert_eq!(layout.mask, None);
        // A brace on a line of its own, and unknown keys, are fine.
        let odd = PIXEL_9_PRO.replace("parts {", "parts\n{\n  colour red").replace("corner_radius 109", "corner_radius 109\n      shape squircle");
        assert_eq!(parse_layout(&odd).unwrap().display, SkinRect { x: 60, y: 61, w: 1280, h: 2856 });
    }

    #[test]
    fn a_malformed_layout_is_no_frame() {
        for bad in [
            "",
            "not a layout",
            &PIXEL_9_PRO.replacen('}', "", 1),
            &format!("{PIXEL_9_PRO}}}"),
            &PIXEL_9_PRO.replace("width 1280", "width -1280"),
            &PIXEL_9_PRO.replace("width 1280", "width 0"),
            &PIXEL_9_PRO.replace("height 2856", "height lots"),
            // The display past the layout's edge.
            &PIXEL_9_PRO.replace("x 60", "x 200"),
            &PIXEL_9_PRO.replace("name device", "name nothing"),
            &PIXEL_9_PRO.replace("layouts {", "elsewhere {"),
            &"a {\n".repeat(10_000),
            &"}\n".repeat(3),
        ] {
            assert_eq!(parse_layout(bad), None, "{bad:.60}");
        }
        // Only a turned layout: nothing upright to draw.
        let turned = PIXEL_9_PRO.replace("y 61\n", "y 61\n      rotation 1\n");
        assert_eq!(parse_layout(&turned), None);
        // A negative corner is no corner.
        let corner = PIXEL_9_PRO.replace("corner_radius 109", "corner_radius -4");
        assert_eq!(parse_layout(&corner).unwrap().corner_radius, None);
    }

    #[test]
    fn a_skin_is_found_by_path_or_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let skins = dir.path().join("skins");
        let skin = install_skin(&skins, "pixel_9_pro");
        // Absolute, as the Pixel 9 Pro AVD has it.
        let found = skin_from(&config(&skin.display().to_string(), 1280, 2856), None).unwrap();
        assert_eq!(found.back, skin.join("back.webp"));
        assert_eq!(found.mask, Some(skin.join("mask.webp")));
        assert_eq!(found.layout.display.w, 1280);
        // A name under the SDK's skins.
        let named = skin_from(&config("pixel_9_pro", 1280, 2856), Some(&skins)).unwrap();
        assert_eq!(named, found);
        // A name with no SDK to look in.
        assert_eq!(skin_from(&config("pixel_9_pro", 1280, 2856), None), None);
        // No mask: still a frame.
        std::fs::remove_file(skin.join("mask.webp")).unwrap();
        assert_eq!(skin_from(&config("pixel_9_pro", 1280, 2856), Some(&skins)).unwrap().mask, None);
        // No background: no frame.
        std::fs::remove_file(skin.join("back.webp")).unwrap();
        assert_eq!(skin_from(&config("pixel_9_pro", 1280, 2856), Some(&skins)), None);
    }

    #[test]
    fn a_skin_gives_the_frame() {
        let dir = tempfile::tempdir().unwrap();
        let skin = install_skin(dir.path(), "pixel_9_pro");
        let spec = spec_with(&config(&skin.display().to_string(), 1280, 2856), None).unwrap();
        assert_eq!(spec.screen_pt, Some((1280.0, 2856.0)));
        assert_eq!(spec.screen_corner_pt, Some(109.0));
        // Frame off, the corners are rounded by the skin's radius.
        assert_eq!(spec.mask, Some(ScreenMask::Rounded));
        assert_eq!(
            spec.frame,
            Some(FrameArt::Skin {
                back: skin.join("back.webp"),
                back_at: (0.0, 0.0),
                foreground: Some(skin.join("mask.webp")),
                corner: Some(109.0),
                // The stand-in foreground is clear: the skin's radius.
                shown_corner: 109.0,
                sizing: Insets { left: 60.0, top: 61.0, right: 68.0, bottom: 57.0 },
            })
        );
        // The art decodes (WebP is built in).
        assert_eq!(decode_art(&skin.join("back.webp")).unwrap().dimensions(), (176, 371));
        assert_eq!(decode_art(&skin.join("layout")), None);
    }

    #[test]
    fn missing_skin_falls_back_to_rounded_screen() {
        // Medium_Phone_API_35: a size, not a skin (skin.dynamic = yes).
        let medium = config("1080x2400", 1080, 2400);
        let rounded = ChromeSpec {
            screen_pt: Some((1080.0, 2400.0)),
            screen_corner_pt: Some(1080.0 * DEFAULT_CORNER),
            mask: Some(ScreenMask::Rounded),
            frame: None,
        };
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(spec_with(&medium, Some(dir.path())), Some(rounded.clone()));
        assert_eq!(spec_with(&medium, None), Some(rounded.clone()));
        // A skin dir that's gone, or has no layout, or a broken one.
        let gone = config(&dir.path().join("gone").display().to_string(), 1080, 2400);
        assert_eq!(spec_with(&gone, None), Some(rounded.clone()));
        let skin = install_skin(dir.path(), "broken");
        std::fs::write(skin.join("layout"), "parts {").unwrap();
        let broken = config(&skin.display().to_string(), 1080, 2400);
        assert_eq!(spec_with(&broken, None), Some(rounded.clone()));
        // A skin for another screen size than the AVD's: its frame wouldn't
        // fit the picture, so the rounded screen.
        let other = install_skin(dir.path(), "pixel_9_pro");
        let resized = config(&other.display().to_string(), 1080, 2400);
        assert_eq!(spec_with(&resized, None), Some(rounded));
        // A skin with no corner radius: the default, on its display.
        std::fs::write(other.join("layout"), PIXEL_9_PRO.replace("corner_radius 109", "")).unwrap();
        let spec = spec_with(&config(&other.display().to_string(), 1280, 2856), None).unwrap();
        assert_eq!(spec.screen_corner_pt, Some(1280.0 * DEFAULT_CORNER));
        // Framed, its art alone shapes the screen: no radius for the frame.
        assert!(matches!(spec.frame, Some(FrameArt::Skin { corner: None, .. })));
        // No skin and no screen size: nothing to draw with.
        assert_eq!(spec_with("skin.path = 1080x2400\n", None), None);
        assert_eq!(spec_with("", None), None);
    }

    /// A `w` × `h` foreground opaque over the corners outside circles of
    /// `r` and clear inside them, but clear too the corners' first pixels
    /// (`clear`, across), as the Pixel 9 Pro's is.
    fn corner_mask(w: u32, h: u32, r: f32, clear: u32) -> image::RgbaImage {
        let keep = crate::devices::chrome::outside_corners(w as usize, h as usize, r);
        image::RgbaImage::from_fn(w, h, |x, y| {
            let into = x.min(w - 1 - x) + y.min(h - 1 - y);
            let a = if into < clear { 0 } else { keep[(y * w + x) as usize] };
            image::Rgba([0, 0, 0, a])
        })
    }

    #[test]
    fn the_corners_a_foreground_shows_are_measured() {
        let near = |got: Option<f32>, want: f32, by: f32| got.is_some_and(|r| (r - want).abs() <= by);
        // Opaque over the corners outside 60, from the corner or past a
        // clear sliver of it: corners of 60.
        for clear in [0, 10] {
            let got = shown_corner(Some(&corner_mask(400, 800, 60.0, clear)), 400, 0.0);
            assert!(near(got, 60.0, 2.0), "{clear}: {got:?}");
        }
        let sixty = corner_mask(400, 800, 60.0, 0);
        // The larger of the art's and the cut's.
        assert!(near(shown_corner(Some(&sixty), 400, 30.0), 60.0, 2.0));
        assert_eq!(shown_corner(Some(&sixty), 400, 109.0), Some(109.0));
        // The roundest corner of the four.
        let top = corner_mask(400, 800, 30.0, 0);
        let mixed = image::RgbaImage::from_fn(400, 800, |x, y| {
            if y < 400 { *top.get_pixel(x, y) } else { *sixty.get_pixel(x, y) }
        });
        assert!(near(shown_corner(Some(&mixed), 400, 0.0), 60.0, 2.0));
        // Art stretched over a display twice its size: in the display's
        // pixels.
        let half = corner_mask(200, 400, 30.0, 0);
        assert!(near(shown_corner(Some(&half), 400, 0.0), 60.0, 4.0));
        // A foreground clear at the corners, or none: the cut.
        let clear = image::RgbaImage::new(400, 800);
        assert_eq!(shown_corner(Some(&clear), 400, 109.0), Some(109.0));
        assert_eq!(shown_corner(Some(&clear), 400, 0.0), Some(0.0));
        assert_eq!(shown_corner(None, 400, 109.0), Some(109.0));
        assert_eq!(shown_corner(None, 400, 0.0), Some(0.0));
        // Art opaque all the way in, or empty: not measured.
        let opaque = image::RgbaImage::from_pixel(400, 800, image::Rgba([0, 0, 0, 255]));
        assert_eq!(shown_corner(Some(&opaque), 400, 109.0), None);
        assert_eq!(shown_corner(Some(&image::RgbaImage::new(0, 0)), 400, 0.0), None);
    }

    #[test]
    fn a_framed_skins_shown_corners_come_from_its_art() {
        let dir = tempfile::tempdir().unwrap();
        let skin = install_skin(dir.path(), "pixel_9_pro");
        let avd = config(&skin.display().to_string(), 1280, 2856);
        let shown = || match spec_with(&avd, None).unwrap().frame {
            Some(FrameArt::Skin { shown_corner, .. }) => shown_corner,
            other => panic!("{other:?}"),
        };
        let mask = skin.join("mask.webp");
        // Each write a second later, so the decoded art isn't kept.
        let mut at = SystemTime::now();
        let mut write = |written: Option<&image::RgbaImage>| {
            match written {
                Some(art) => art.save_with_format(&mask, image::ImageFormat::WebP).unwrap(),
                None => std::fs::write(&mask, b"not webp").unwrap(),
            }
            at += std::time::Duration::from_secs(1);
            std::fs::File::options().write(true).open(&mask).unwrap().set_modified(at).unwrap();
        };
        // Art at half the display's size rounding it to 171: 171, past
        // the skin's 109.
        write(Some(&corner_mask(640, 1428, 171.0 / 2.0, 8)));
        let got = shown();
        assert!((got - 171.0).abs() <= 4.0, "{got}");
        // Art that can't be read: the screen's own corners.
        write(None);
        assert_eq!(shown(), 109.0);
        // No radius in the layout: the art alone, where it rounds little.
        std::fs::write(skin.join("layout"), PIXEL_9_PRO.replace("corner_radius 109", "")).unwrap();
        write(Some(&corner_mask(640, 1428, 31.0 / 2.0, 0)));
        let got = shown();
        assert!((got - 31.0).abs() <= 4.0, "{got}");
        // And no foreground either: square, as the frame shows it.
        std::fs::remove_file(&mask).unwrap();
        assert_eq!(shown(), 0.0);
    }

    /// Manual, on the SDK's own art: `cargo test measures_the_sdks_skins --
    /// --ignored --nocapture`. Prints each skin's radius and the corners its
    /// frame shows; the Pixel 9 Pro's art rounds its screen to about 177.
    #[test]
    #[ignore]
    fn measures_the_sdks_skins() {
        let skins = super::super::sdk::sdk().expect("an Android SDK").join("skins");
        let mut pixel_9_pro = None;
        for name in ["pixel_9_pro", "pixel_6", "pixel_7", "pixel_tablet", "nexus_5"] {
            let skin = skin_from(&format!("skin.path = {name}\n"), Some(&skins)).expect(name);
            let Some(FrameArt::Skin { corner, shown_corner, .. }) = skin.spec().frame else {
                panic!("{name}: no frame");
            };
            println!("{name}: corner_radius {corner:?}, mask {:?}, shown {shown_corner:.1}", skin.mask.is_some());
            if name == "pixel_9_pro" {
                pixel_9_pro = Some(shown_corner);
            }
        }
        // Its foreground covers 52 pixels in along each diagonal (clear for
        // the first 8 or so, which the skin's 109 cuts): a circle of 177.
        let got = pixel_9_pro.unwrap();
        assert!((170.0..=185.0).contains(&got), "{got}");
    }

    #[test]
    fn decoded_art_is_kept_until_its_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (dir.path().join("a.webp"), dir.path().join("b.webp"), dir.path().join("c.webp"));
        for p in [&a, &b, &c] {
            write_webp(p, 4, 4, [1, 2, 3, 255]);
        }
        let mut cache = ArtCache::new();
        let decodes = std::cell::Cell::new(0);
        let decode = |p: &Path| {
            decodes.set(decodes.get() + 1);
            decode_art(p)
        };
        let first = cache.get(&a, decode).unwrap();
        assert!(Arc::ptr_eq(&first, &cache.get(&a, decode).unwrap()));
        cache.get(&b, decode).unwrap();
        assert!(Arc::ptr_eq(&first, &cache.get(&a, decode).unwrap()));
        assert_eq!(decodes.get(), 2);
        // A third pushes out the one used longest ago (b).
        cache.get(&c, decode).unwrap();
        cache.get(&a, decode).unwrap();
        assert_eq!(decodes.get(), 3);
        cache.get(&b, decode).unwrap();
        assert_eq!(decodes.get(), 4);
        // A changed file is decoded again.
        write_webp(&b, 2, 2, [9, 9, 9, 255]);
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&b).unwrap().set_modified(later).unwrap();
        assert_eq!(cache.get(&b, decode).unwrap().dimensions(), (2, 2));
        assert_eq!(decodes.get(), 5);
        // Gone, or not art: nothing, and nothing kept.
        assert!(cache.get(&dir.path().join("gone.webp"), decode).is_none());
        std::fs::write(&c, b"not webp").unwrap();
        std::fs::File::options().write(true).open(&c).unwrap().set_modified(later).unwrap();
        assert!(cache.get(&c, decode).is_none());
        assert!(art(&dir.path().join("gone.webp")).is_none());
    }

    #[test]
    fn an_avds_chrome_is_read_from_its_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skin = install_skin(dir.path(), "pixel_9_pro");
        let avd = dir.path().join("Pixel_9_Pro_API_35.avd");
        std::fs::create_dir_all(&avd).unwrap();
        std::fs::write(avd.join("config.ini"), config(&skin.display().to_string(), 1280, 2856)).unwrap();
        assert!(chrome_for_avd(&avd).unwrap().frame.is_some());
        assert_eq!(skin_for(&avd).unwrap().back, skin.join("back.webp"));
        assert_eq!(chrome_for_avd(&dir.path().join("none.avd")), None);
        assert_eq!(chrome_key(&avd), format!("android:{}", avd.display()));
    }

    /// Manual, to look at the frame (needs an AVD with a skin, such as the
    /// Pixel 9 Pro's): `OO_FRAME_AVD=<…/Pixel_9_Pro_API_35.avd>
    /// OO_FRAME_PNG_DIR=<dir> cargo test draws_the_skin_for_a_look --
    /// --ignored`. Paints it as the Device tab does (layout, device-pixel
    /// snapping, the overlay over the picture), with a gradient standing in
    /// for the screen, on a light and a dark panel: portrait, Landscape and
    /// ReversePortrait framed, and portrait with the frame off. The focus
    /// ring goes over it in green, as the Device tab lays it out; each
    /// screen's top-left and bottom-right corners are saved again 4× up,
    /// and the framed portrait's too with the ring the screen's own
    /// corners would give (`-before`).
    #[test]
    #[ignore]
    fn draws_the_skin_for_a_look() {
        use crate::devices::Quarter;
        use crate::devices::chrome::{
            OverlayKey, Rect, focus_ring, painted, place, render, snap, turned_size,
        };
        let avd = PathBuf::from(std::env::var("OO_FRAME_AVD").expect("set OO_FRAME_AVD"));
        let out = PathBuf::from(std::env::var("OO_FRAME_PNG_DIR").expect("set OO_FRAME_PNG_DIR"));
        let spec = chrome_for_avd(&avd).expect("the AVD's chrome");
        let geometry = spec.geometry().expect("a skin with a frame");
        let light = [236u8, 238, 242, 255];
        let dark = [40u8, 44, 52, 255];
        for (name, turn, framed, scale, panel) in [
            ("portrait-light", Quarter::None, true, 2.0f32, light),
            ("portrait-dark", Quarter::None, true, 1.5, dark),
            ("landscape-light", Quarter::Cw270, true, 2.0, light),
            ("reverse-portrait-dark", Quarter::Cw180, true, 2.0, dark),
            ("frame-off-light", Quarter::None, false, 2.0, light),
        ] {
            let (fw, fh) = geometry.frame_pt(turn);
            // A panel about as wide as the user's, the frame fitted in it.
            let width = if matches!(turn, Quarter::None | Quarter::Cw180) { 356.3 } else { 731.7 };
            let container = Rect { x: 8.25, y: 3.4, w: width, h: width * fh / fw + 9.3 };
            let picture = turned_size(geometry.screen_pt, turn);
            let shown = framed.then_some(geometry);
            let p = painted(place(container, picture, shown, turn), shown, turn, scale);
            let key = OverlayKey {
                device_type: chrome_key(&avd),
                size_px: p.overlay_px,
                turn,
                framed,
                fill: panel,
            };
            let overlay = render::overlay(&spec, &key).expect("an overlay");
            let all = snap(container, scale);
            let (od, pd) = (snap(p.overlay, scale), snap(p.picture, scale));
            let (w, h) = ((all.x1 + 8) as u32, (all.y1 + 8) as u32);
            let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba(panel));
            for (x, y, px) in img.enumerate_pixels_mut() {
                let (x, y) = (i64::from(x), i64::from(y));
                if x >= pd.x0 && x < pd.x1 && y >= pd.y0 && y < pd.y1 {
                    // A bright gradient for the screen, its top left red.
                    let u = (x - pd.x0) as f32 / (pd.x1 - pd.x0) as f32;
                    let v = (y - pd.y0) as f32 / (pd.y1 - pd.y0) as f32;
                    px.0 = [(255.0 * (1.0 - u)) as u8, (255.0 * v) as u8, 230, 255];
                }
                if x >= od.x0 && x < od.x1 && y >= od.y0 && y < od.y1 {
                    // The overlay, BGRA with straight alpha, at its own size.
                    let o = overlay.get_pixel((x - od.x0) as u32, (y - od.y0) as u32).0;
                    let a = f32::from(o[3]) / 255.0;
                    let under = px.0;
                    let mix = |top: u8, bottom: u8| (f32::from(top) * a + f32::from(bottom) * (1.0 - a)).round() as u8;
                    px.0 = [mix(o[2], under[0]), mix(o[1], under[1]), mix(o[0], under[2]), 255];
                }
            }
            // The focus ring, 2 wide inside its rect as gpui's border is,
            // with the corner the Device tab gives it, and with the screen's
            // own (what it had before following the frame).
            let ring_over = |img: &mut image::RgbaImage, corner: Option<f32>| {
                let (ring, radius) =
                    focus_ring(p.picture, container, spec.screen_pt, corner, turn, 2.0);
                let (x0, y0) = (ring.x * scale, ring.y * scale);
                let (x1, y1) = ((ring.x + ring.w) * scale, (ring.y + ring.h) * scale);
                let (r, stroke) = (radius * scale, 2.0 * scale);
                for (x, y, px) in img.enumerate_pixels_mut() {
                    let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let qx = (x0 + r - cx).max(cx - (x1 - r)).max(0.0);
                    let qy = (y0 + r - cy).max(cy - (y1 - r)).max(0.0);
                    // How far inside the ring's outer edge.
                    let inside = if qx > 0.0 && qy > 0.0 {
                        r - (qx * qx + qy * qy).sqrt()
                    } else {
                        (cx - x0).min(x1 - cx).min(cy - y0).min(y1 - cy)
                    };
                    if (0.0..stroke).contains(&inside) {
                        px.0 = [0, 255, 60, 255];
                    }
                }
            };
            let crops = |img: &image::RgbaImage, tag: &str| {
                let side = (spec.ring_corner(framed).unwrap_or(0.0) * p.picture.w
                    / turned_size(geometry.screen_pt, turn).0
                    * scale
                    * 1.3) as u32
                    + 24;
                let (x0, y0) = ((pd.x0 - 12).max(0) as u32, (pd.y0 - 12).max(0) as u32);
                let (x1, y1) = ((pd.x1 + 12) as u32 - side, (pd.y1 + 12) as u32 - side);
                for (at, (x, y)) in [("tl", (x0, y0)), ("br", (x1, y1))] {
                    let crop = image::imageops::crop_imm(img, x, y, side, side).to_image();
                    let big = image::imageops::resize(&crop, side * 4, side * 4, image::imageops::FilterType::Nearest);
                    big.save(out.join(format!("ring-{name}-{at}{tag}.png"))).unwrap();
                }
            };
            if name == "portrait-light" {
                let mut before = img.clone();
                ring_over(&mut before, spec.screen_corner_pt);
                crops(&before, "-before");
            }
            ring_over(&mut img, spec.ring_corner(framed));
            crops(&img, "");
            img.save(out.join(format!("frame-{name}.png"))).unwrap();
        }
    }
}
