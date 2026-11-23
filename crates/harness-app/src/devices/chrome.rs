//! The simulator's device frame ("chrome") and screen mask, read at run time
//! from the user's Xcode install. Nothing of Apple's is copied into the repo
//! or the app.
//!
//! Where each piece comes from, as found on Xcode 27.2 for the iPhone 18 Pro
//! (chrome `phone11`):
//!
//! - `xcrun simctl list devicetypes -j`: each type's `identifier`
//!   (`com.apple.CoreSimulator.SimDeviceType.iPhone-18-Pro`) and
//!   `bundlePath` (`…/DeviceTypes/iPhone 18 Pro.simdevicetype`).
//! - `<bundle>/Contents/Resources/profile.plist`: `chromeIdentifier`
//!   (`com.apple.dt.devicekit.chrome.phone11`) and `framebufferMask`, the
//!   name of a PDF beside it whose shape is the screen's (rounded corners).
//! - `<bundle>/Contents/Resources/capabilities.plist`:
//!   `capabilities.ScreenDimensionsCapability.main-screen-{width,height,scale}`
//!   (1206 × 2622 px at 3×): the screen in points, which sets the frame's
//!   thickness against the picture. The mask PDF's own size is no unit (it
//!   is pixels on some devices, points or 6× on others), so the mask is
//!   always stretched over the screen.
//! - `/Library/Developer/DeviceKit/Chrome/phone11.devicechrome/Contents/Resources/chrome.json`:
//!   `images.{topLeft,top,topRight,right,bottomRight,bottom,bottomLeft,left}`
//!   name the nine-slice PDFs beside it, and
//!   `images.sizing.{leftWidth,topHeight,rightWidth,bottomHeight}` (18 each
//!   for phone11) is how far the frame reaches past the screen on each side,
//!   in points. So the frame is the screen plus those insets: 438 × 910 pt
//!   around a 402 × 874 pt screen.
//!
//! `images.composite`, the whole frame in one PDF, is drawn (stretched over
//! the frame) when it fits it within a few points ([`composite_fits`]):
//! phone11's is 436 × 908 for 438 × 910. Some chromes' slices are only
//! placeholders (phone14 and phone15, the Duo's, are red "unused" boxes) and
//! their composite is the art. Otherwise the nine slices: a chrome is shared
//! by devices with different screens (phone12's composite fits the 18 Pro
//! Max, not the Air), and the older chromes have no composite. Corners are
//! drawn at their size and edges stretched between them, which fits any
//! screen.
//!
//! gpui can't clip a surface to an image mask, so the frame is one overlay
//! drawn over the picture: the frame with the mask's shape cut out of it, or
//! with the frame off, the panel's colour with the same hole (it covers the
//! picture's corners). Parsing and layout are all-platform; rendering
//! (CoreGraphics) is macOS only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use super::Quarter;

const CHROME_ROOT: &str = "/Library/Developer/DeviceKit/Chrome";
const TYPE_PREFIX: &str = "com.apple.CoreSimulator.SimDeviceType.";
const TIMEOUT: Duration = Duration::from_secs(5);

/// A rectangle in logical pixels, origin top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// How far the frame reaches past the screen on each side, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Insets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Insets {
    /// The insets once the portrait frame is turned clockwise by `turn`.
    pub(crate) fn turned(self, turn: Quarter) -> Insets {
        let Insets { left, top, right, bottom } = self;
        match turn {
            Quarter::None => self,
            // Clockwise, the left side goes to the top.
            Quarter::Cw90 => Insets { left: bottom, top: left, right: top, bottom: right },
            Quarter::Cw180 => Insets { left: right, top: bottom, right: left, bottom: top },
            Quarter::Cw270 => Insets { left: top, top: right, right: bottom, bottom: left },
        }
    }
}

/// `(w, h)` turned by `turn`.
pub(crate) fn turned_size((w, h): (f32, f32), turn: Quarter) -> (f32, f32) {
    match turn {
        Quarter::Cw90 | Quarter::Cw270 => (h, w),
        Quarter::None | Quarter::Cw180 => (w, h),
    }
}

/// The nine-slice PDFs' names (no `.pdf`), from `chrome.json`'s `images`.
// Read only by the macOS renderer.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NineSlice {
    pub top_left: String,
    pub top: String,
    pub top_right: String,
    pub right: String,
    pub bottom_right: String,
    pub bottom: String,
    pub bottom_left: String,
    pub left: String,
}

/// A device chrome: where its PDFs are, which they are, and its insets.
// Read only by the macOS renderer.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FrameArt {
    pub dir: PathBuf,
    pub pieces: NineSlice,
    /// `images.composite`, the whole frame in one PDF, if the chrome has one.
    pub composite: Option<String>,
    pub sizing: Insets,
}

/// What the Device tab draws a device type with. A missing part leaves its
/// look out: no frame means the rounded screen alone, no mask too means the
/// picture as it comes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChromeSpec {
    /// The screen in points, portrait.
    pub screen_pt: Option<(f32, f32)>,
    /// The `framebufferMask` PDF.
    pub mask: Option<PathBuf>,
    /// Only with `screen_pt`: the frame is laid out around it.
    pub frame: Option<FrameArt>,
}

impl ChromeSpec {
    /// The frame's layout, when there is a frame.
    pub(crate) fn geometry(&self) -> Option<FrameGeometry> {
        Some(FrameGeometry { screen_pt: self.screen_pt?, sizing: self.frame.as_ref()?.sizing })
    }
}

/// `profile.plist`'s two names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Profile {
    pub chrome: Option<String>,
    pub mask: Option<String>,
}

/// The nine slices, the insets and the composite (if any) in a
/// `chrome.json`; None unless the slices and insets are all there.
pub(crate) fn parse_chrome_json(json: &str) -> Option<(NineSlice, Insets, Option<String>)> {
    let root: Value = serde_json::from_str(json).ok()?;
    let images = root.get("images")?;
    let name = |k: &str| images.get(k).and_then(Value::as_str).map(str::to_string);
    let pieces = NineSlice {
        top_left: name("topLeft")?,
        top: name("top")?,
        top_right: name("topRight")?,
        right: name("right")?,
        bottom_right: name("bottomRight")?,
        bottom: name("bottom")?,
        bottom_left: name("bottomLeft")?,
        left: name("left")?,
    };
    let sizing = images.get("sizing")?;
    let side = |k: &str| sizing.get(k).and_then(Value::as_f64).map(|v| v as f32);
    let insets = Insets {
        left: side("leftWidth")?,
        top: side("topHeight")?,
        right: side("rightWidth")?,
        bottom: side("bottomHeight")?,
    };
    Some((pieces, insets, name("composite")))
}

/// `profile.plist` (as JSON): its chrome and mask names, either missing.
pub(crate) fn parse_profile(json: &str) -> Option<Profile> {
    let root: Value = serde_json::from_str(json).ok()?;
    let name = |k: &str| root.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Profile { chrome: name("chromeIdentifier"), mask: name("framebufferMask") })
}

/// `capabilities.plist` (as JSON): the main screen in points.
pub(crate) fn parse_screen_points(json: &str) -> Option<(f32, f32)> {
    let root: Value = serde_json::from_str(json).ok()?;
    let dims = root.get("capabilities")?.get("ScreenDimensionsCapability")?;
    let get = |k: &str| dims.get(k).and_then(Value::as_f64);
    let scale = get("main-screen-scale").filter(|s| *s > 0.0)?;
    let (w, h) = (get("main-screen-width")?, get("main-screen-height")?);
    (w > 0.0 && h > 0.0).then(|| ((w / scale) as f32, (h / scale) as f32))
}

/// `simctl list devicetypes -j`: each type's bundle, keyed by the
/// identifier's suffix (`iPhone-18-Pro`, as `DeviceInfo::device_type`).
pub(crate) fn parse_device_type_bundles(json: &str) -> Option<HashMap<String, PathBuf>> {
    let root: Value = serde_json::from_str(json).ok()?;
    let types = root.get("devicetypes")?.as_array()?;
    Some(
        types
            .iter()
            .filter_map(|t| {
                let id = t.get("identifier")?.as_str()?;
                let path = t.get("bundlePath")?.as_str()?;
                Some((id.strip_prefix(TYPE_PREFIX).unwrap_or(id).to_string(), PathBuf::from(path)))
            })
            .collect(),
    )
}

/// A device type's spec from its `bundle`, with chromes under
/// `chrome_root`. `read_plist` gives a plist as JSON. None when the profile
/// can't be read at all.
pub(crate) fn spec_from(
    bundle: &Path,
    chrome_root: &Path,
    read_plist: impl Fn(&Path) -> Option<String>,
) -> Option<ChromeSpec> {
    let res = bundle.join("Contents/Resources");
    let profile = parse_profile(&read_plist(&res.join("profile.plist"))?)?;
    let screen_pt = read_plist(&res.join("capabilities.plist"))
        .as_deref()
        .and_then(parse_screen_points);
    let mask = profile
        .mask
        .map(|name| res.join(format!("{name}.pdf")))
        .filter(|p| p.is_file());
    let frame = profile.chrome.filter(|_| screen_pt.is_some()).and_then(|id| {
        // "com.apple.dt.devicekit.chrome.phone11" -> "phone11.devicechrome".
        let short = id.rsplit('.').next()?;
        let dir = chrome_root.join(format!("{short}.devicechrome/Contents/Resources"));
        let json = std::fs::read_to_string(dir.join("chrome.json")).ok()?;
        let (pieces, sizing, composite) = parse_chrome_json(&json)?;
        Some(FrameArt { dir, pieces, composite, sizing })
    });
    Some(ChromeSpec { screen_pt, mask, frame })
}

/// A plist as JSON, via `plutil` (macOS ships it; elsewhere this is None).
fn read_plist(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    let args = ["-convert", "json", "-o", "-", path];
    let ran = crate::ports::run_bounded_why("plutil", &args, TIMEOUT).ok()?;
    (ran.code == Some(0)).then_some(ran.stdout)
}

/// The bundle of `device_type_id`. The device type list is read once per
/// run (it changes only with Xcode); a failed read is tried again next time.
fn bundle_for(device_type_id: &str) -> Option<PathBuf> {
    static BUNDLES: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);
    let mut bundles = BUNDLES.lock().unwrap_or_else(|e| e.into_inner());
    if bundles.is_none() {
        let listed = super::simctl::simctl(&["list", "devicetypes", "-j"]).ok();
        *bundles = listed.as_deref().and_then(parse_device_type_bundles);
    }
    bundles.as_ref()?.get(device_type_id).cloned()
}

/// The frame, mask and screen of `device_type_id` (`iPhone-18-Pro`), read
/// once per type per run. Blocks on `simctl` and `plutil`: off the UI
/// thread only.
pub(crate) fn chrome_for(device_type_id: &str) -> Option<Arc<ChromeSpec>> {
    static SPECS: Mutex<Option<HashMap<String, Option<Arc<ChromeSpec>>>>> = Mutex::new(None);
    if let Some(known) = SPECS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .get(device_type_id)
    {
        return known.clone();
    }
    // The type list couldn't be read: try again next time.
    let bundle = bundle_for(device_type_id)?;
    let spec = spec_from(&bundle, Path::new(CHROME_ROOT), read_plist).map(Arc::new);
    SPECS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(device_type_id.to_string(), spec.clone());
    spec
}

/// How far, in points, a composite may differ from the frame it is drawn
/// over in either direction.
const COMPOSITE_SLACK_PT: f32 = 4.0;

/// Whether a composite of `composite` points can stand for a frame of
/// `frame` points (portrait both).
pub(crate) fn composite_fits(composite: (f32, f32), frame: (f32, f32)) -> bool {
    (composite.0 - frame.0).abs() <= COMPOSITE_SLACK_PT
        && (composite.1 - frame.1).abs() <= COMPOSITE_SLACK_PT
}

/// What the frame's layout needs: the portrait screen in points and the
/// frame's insets around it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FrameGeometry {
    pub screen_pt: (f32, f32),
    pub sizing: Insets,
}

impl FrameGeometry {
    /// The whole frame in points, turned by `turn`.
    pub(crate) fn frame_pt(&self, turn: Quarter) -> (f32, f32) {
        let (w, h) = turned_size(self.screen_pt, turn);
        let i = self.sizing.turned(turn);
        (w + i.left + i.right, h + i.top + i.bottom)
    }
}

/// Where the frame (if drawn) and the picture go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub frame: Option<Rect>,
    pub screen: Rect,
}

/// `size` scaled to fit `container` whole and centred in it.
fn contain(container: Rect, (w, h): (f32, f32)) -> Rect {
    if w <= 0.0 || h <= 0.0 {
        return Rect { x: container.x, y: container.y, w: 0.0, h: 0.0 };
    }
    let scale = (container.w / w).min(container.h / h).max(0.0);
    let (w, h) = (w * scale, h * scale);
    Rect { x: container.x + (container.w - w) / 2.0, y: container.y + (container.h - h) / 2.0, w, h }
}

/// Lay the picture out in `container`: inside its frame (`frame`, turned
/// by `turn` like the picture) fitted to the box, or with no frame, the
/// picture (`picture`, its size upright) fitted alone.
pub(crate) fn place(
    container: Rect,
    picture: (f32, f32),
    frame: Option<FrameGeometry>,
    turn: Quarter,
) -> Placement {
    match frame {
        None => Placement { frame: None, screen: contain(container, picture) },
        Some(geometry) => {
            let outer = contain(container, geometry.frame_pt(turn));
            Placement { frame: Some(outer), screen: screen_rect_in_frame(outer, geometry, turn) }
        }
    }
}

/// The screen's rectangle within a frame drawn at `frame` (turned by
/// `turn`): where the picture goes, and what a click on it maps from.
pub(crate) fn screen_rect_in_frame(frame: Rect, geometry: FrameGeometry, turn: Quarter) -> Rect {
    let (sw, sh) = turned_size(geometry.screen_pt, turn);
    let (fw, _) = geometry.frame_pt(turn);
    let i = geometry.sizing.turned(turn);
    let s = if fw > 0.0 { frame.w / fw } else { 0.0 };
    Rect { x: frame.x + i.left * s, y: frame.y + i.top * s, w: sw * s, h: sh * s }
}

/// What one overlay is drawn for. Equal keys draw the same image.
// Read only by the macOS renderer.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverlayKey {
    pub device_type: String,
    /// Its size in device pixels, turned.
    pub size_px: (u32, u32),
    pub turn: Quarter,
    /// The frame with the screen cut out, or (false) the panel colour
    /// with the screen cut out, covering the picture's corners.
    pub framed: bool,
    /// The panel's colour (RGB; alpha unused), which covers the picture's
    /// corners wherever no frame does.
    pub fill: [u8; 4],
}

/// Draw the overlay `key` asks for (BGRA, straight alpha, as gpui's sprites
/// take it). None when a PDF it needs can't be read, or there is nothing to
/// draw: no frame for a framed key, no mask for one that isn't.
#[cfg(target_os = "macos")]
pub(crate) fn render_overlay(
    spec: &ChromeSpec,
    key: &OverlayKey,
) -> Option<Arc<gpui::RenderImage>> {
    let bgra = render::overlay(spec, key)?;
    Some(Arc::new(gpui::RenderImage::new(vec![image::Frame::new(bgra)])))
}

/// Elsewhere there is no simulator to draw.
#[cfg(not(target_os = "macos"))]
pub(crate) fn render_overlay(
    _spec: &ChromeSpec,
    _key: &OverlayKey,
) -> Option<Arc<gpui::RenderImage>> {
    None
}

/// The overlay in pixels, with CoreGraphics. Drawn portrait, then turned.
#[cfg(target_os = "macos")]
pub(crate) mod render {
    use std::ffi::c_void;
    use std::path::Path;

    use image::RgbaImage;
    use objc2_core_foundation::{CFRetained, CFURL, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
        CGPDFBox, CGPDFDocument, CGPDFPage,
    };

    use super::{ChromeSpec, FrameArt, OverlayKey, Quarter, Rect, screen_rect_in_frame, turned_size};

    /// A page of a PDF, and its size in points.
    struct Pdf {
        page: CFRetained<CGPDFPage>,
        bounds: CGRect,
    }

    fn open(path: &Path) -> Option<Pdf> {
        let url = CFURL::from_file_path(path)?;
        let doc = CGPDFDocument::with_url(Some(&url))?;
        let page = CGPDFDocument::page(Some(&doc), 1)?;
        let bounds = CGPDFPage::box_rect(Some(&page), CGPDFBox::MediaBox);
        (bounds.size.width > 0.0 && bounds.size.height > 0.0).then_some(Pdf { page, bounds })
    }

    /// A blank BGRA bitmap, premultiplied (all CoreGraphics draws into),
    /// and its context drawing into `pixels`.
    struct Canvas {
        pixels: Vec<u8>,
        w: usize,
        h: usize,
        ctx: CFRetained<CGContext>,
    }

    impl Canvas {
        fn new(w: usize, h: usize) -> Option<Canvas> {
            let mut pixels = vec![0u8; w.checked_mul(h)?.checked_mul(4)?];
            let space = CGColorSpace::new_device_rgb()?;
            // BGRA in memory: alpha first, 32-bit little-endian.
            let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
            // SAFETY: `pixels` holds h rows of w * 4 bytes and outlives the
            // context (both live in the Canvas, dropped together; the
            // context is only drawn into while the Canvas lives).
            let ctx = unsafe {
                CGBitmapContextCreate(
                    pixels.as_mut_ptr().cast::<c_void>(),
                    w,
                    h,
                    8,
                    w * 4,
                    Some(&space),
                    info,
                )
            }?;
            Some(Canvas { pixels, w, h, ctx })
        }

        /// Draw `pdf` stretched over `r` (pixels, origin top left).
        fn draw(&self, pdf: &Pdf, r: Rect) {
            if r.w <= 0.0 || r.h <= 0.0 {
                return;
            }
            let ctx = Some(&*self.ctx);
            CGContext::save_g_state(ctx);
            let at = self.cg_rect(r);
            // A page draws past its box unless clipped; stretched, an edge
            // piece's overhang would cover the whole frame.
            CGContext::clip_to_rect(ctx, at);
            CGContext::translate_ctm(ctx, at.origin.x, at.origin.y);
            CGContext::scale_ctm(
                ctx,
                f64::from(r.w) / pdf.bounds.size.width,
                f64::from(r.h) / pdf.bounds.size.height,
            );
            CGContext::translate_ctm(ctx, -pdf.bounds.origin.x, -pdf.bounds.origin.y);
            CGContext::draw_pdf_page(ctx, Some(&pdf.page));
            CGContext::restore_g_state(ctx);
        }

        /// Paint `r` (pixels, origin top left) opaque `colour` (RGB).
        fn fill(&self, r: Rect, [red, green, blue, _]: [u8; 4]) {
            let ctx = Some(&*self.ctx);
            let c = |v: u8| f64::from(v) / 255.0;
            CGContext::set_rgb_fill_color(ctx, c(red), c(green), c(blue), 1.0);
            CGContext::fill_rect(ctx, self.cg_rect(r));
        }

        /// `r` (origin top left) in CoreGraphics' terms (origin bottom left).
        fn cg_rect(&self, r: Rect) -> CGRect {
            let bottom = self.h as f64 - f64::from(r.y + r.h);
            CGRect::new(
                CGPoint::new(f64::from(r.x), bottom),
                CGSize::new(f64::from(r.w), f64::from(r.h)),
            )
        }

        /// The pixels, done drawing.
        fn into_pixels(self) -> Vec<u8> {
            drop(self.ctx);
            self.pixels
        }
    }

    /// Whole pixels for a rect, so neighbouring pieces meet without a seam.
    fn snapped(r: Rect) -> (usize, usize, usize, usize) {
        let x0 = r.x.round().max(0.0) as usize;
        let y0 = r.y.round().max(0.0) as usize;
        let x1 = (r.x + r.w).round().max(0.0) as usize;
        let y1 = (r.y + r.h).round().max(0.0) as usize;
        (x0, y0, x1.max(x0), y1.max(y0))
    }

    /// The nine slices laid out on `canvas` (the whole frame), `s` pixels
    /// per point: corners at their size, edges stretched between them.
    fn draw_frame(canvas: &Canvas, art: &FrameArt, s: f32) -> Option<()> {
        let piece = |name: &str| open(&art.dir.join(format!("{name}.pdf")));
        let p = &art.pieces;
        let (tl, t, tr, r, br, b, bl, l) = (
            piece(&p.top_left)?,
            piece(&p.top)?,
            piece(&p.top_right)?,
            piece(&p.right)?,
            piece(&p.bottom_right)?,
            piece(&p.bottom)?,
            piece(&p.bottom_left)?,
            piece(&p.left)?,
        );
        let (w, h) = (canvas.w as f32, canvas.h as f32);
        let px = |pt: f64| (pt as f32 * s).round();
        let size = |pdf: &Pdf| (px(pdf.bounds.size.width), px(pdf.bounds.size.height));
        let rect = |x, y, w, h| Rect { x, y, w, h };
        let ((tlw, tlh), (trw, trh)) = (size(&tl), size(&tr));
        let ((blw, blh), (brw, brh)) = (size(&bl), size(&br));
        canvas.draw(&tl, rect(0.0, 0.0, tlw, tlh));
        canvas.draw(&tr, rect(w - trw, 0.0, trw, trh));
        canvas.draw(&bl, rect(0.0, h - blh, blw, blh));
        canvas.draw(&br, rect(w - brw, h - brh, brw, brh));
        canvas.draw(&t, rect(tlw, 0.0, w - tlw - trw, size(&t).1));
        let bh = size(&b).1;
        canvas.draw(&b, rect(blw, h - bh, w - blw - brw, bh));
        canvas.draw(&l, rect(0.0, tlh, size(&l).0, h - tlh - blh));
        let rw = size(&r).0;
        canvas.draw(&r, rect(w - rw, trh, rw, h - trh - brh));
        Some(())
    }

    /// Cut `mask` (stretched over `hole`) out of `canvas`'s pixels: each is
    /// kept by (1 − the mask's alpha). No mask cuts the whole rect.
    fn cut(pixels: &mut [u8], width: usize, hole: Rect, mask: Option<&Pdf>) -> Option<()> {
        let (x0, y0, x1, y1) = snapped(hole);
        let (hw, hh) = (x1 - x0, y1 - y0);
        if hw == 0 || hh == 0 {
            return Some(());
        }
        let shape = match mask {
            Some(pdf) => {
                let m = Canvas::new(hw, hh)?;
                m.draw(pdf, Rect { x: 0.0, y: 0.0, w: hw as f32, h: hh as f32 });
                Some(m.into_pixels())
            }
            None => None,
        };
        for y in 0..hh {
            for x in 0..hw {
                let keep = shape.as_ref().map_or(0, |m| 255 - u32::from(m[(y * hw + x) * 4 + 3]));
                let at = ((y0 + y) * width + x0 + x) * 4;
                let Some(px) = pixels.get_mut(at..at + 4) else {
                    continue;
                };
                // Premultiplied: scaling every channel scales the alpha.
                for c in px {
                    *c = ((u32::from(*c) * keep + 127) / 255) as u8;
                }
            }
        }
        Some(())
    }

    /// Premultiplied to straight alpha.
    fn unpremultiply(pixels: &mut [u8]) {
        for px in pixels.as_chunks_mut::<4>().0 {
            let a = u32::from(px[3]);
            if a == 0 || a == 255 {
                continue;
            }
            for c in &mut px[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }

    pub(crate) fn overlay(spec: &ChromeSpec, key: &OverlayKey) -> Option<RgbaImage> {
        let (pw, ph) = turned_size((key.size_px.0 as f32, key.size_px.1 as f32), key.turn);
        let (pw, ph) = (pw as usize, ph as usize);
        if pw == 0 || ph == 0 {
            return None;
        }
        let canvas = Canvas::new(pw, ph)?;
        let mask = match &spec.mask {
            Some(path) => Some(open(path)?),
            None => None,
        };
        let whole = Rect { x: 0.0, y: 0.0, w: pw as f32, h: ph as f32 };
        let hole = if key.framed {
            let art = spec.frame.as_ref()?;
            let geometry = spec.geometry()?;
            let screen = screen_rect_in_frame(whole, geometry, Quarter::None);
            // The screen's square corners reach past the frame's rounded
            // body: the panel's colour covers them there, under the frame.
            let (x0, y0, x1, y1) = snapped(screen);
            let snapped = Rect { x: x0 as f32, y: y0 as f32, w: (x1 - x0) as f32, h: (y1 - y0) as f32 };
            canvas.fill(snapped, key.fill);
            let frame_pt = geometry.frame_pt(Quarter::None);
            let composite = art
                .composite
                .as_ref()
                .and_then(|name| open(&art.dir.join(format!("{name}.pdf"))))
                .filter(|pdf| {
                    let size = (pdf.bounds.size.width as f32, pdf.bounds.size.height as f32);
                    super::composite_fits(size, frame_pt)
                });
            match composite {
                Some(pdf) => canvas.draw(&pdf, whole),
                None => draw_frame(&canvas, art, pw as f32 / frame_pt.0)?,
            }
            screen
        } else {
            mask.as_ref()?;
            canvas.fill(whole, key.fill);
            whole
        };
        let mut pixels = canvas.into_pixels();
        cut(&mut pixels, pw, hole, mask.as_ref())?;
        unpremultiply(&mut pixels);
        let upright = RgbaImage::from_raw(pw as u32, ph as u32, pixels)?;
        Some(match key.turn {
            Quarter::None => upright,
            Quarter::Cw90 => image::imageops::rotate90(&upright),
            Quarter::Cw180 => image::imageops::rotate180(&upright),
            Quarter::Cw270 => image::imageops::rotate270(&upright),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE11: &str = include_str!("testdata/chrome_phone11.json");

    /// `profile.plist` as `plutil -convert json` prints it, trimmed to the
    /// keys read (values as found for the iPhone 18 Pro).
    const PROFILE: &str = r#"{
        "chromeIdentifier": "com.apple.dt.devicekit.chrome.phone11",
        "framebufferMask": "4E5532ED-1470-47D1-BDF4-7AA90C26957A",
        "modelIdentifier": "iPhone19,2"
    }"#;

    /// `capabilities.plist`, trimmed the same way.
    const CAPABILITIES: &str = r#"{
        "capabilities": {
            "ScreenDimensionsCapability": {
                "main-screen-height": 2622,
                "main-screen-scale": 3,
                "main-screen-width": 1206
            }
        }
    }"#;

    #[test]
    fn parses_phone11_chrome_json() {
        let (pieces, sizing, composite) = parse_chrome_json(PHONE11).unwrap();
        assert_eq!(composite.as_deref(), Some("PhoneComposite"));
        assert_eq!(pieces.top_left, "Phone TL");
        assert_eq!(pieces.top, "Phone Top");
        assert_eq!(pieces.top_right, "Phone TR");
        assert_eq!(pieces.right, "Phone Right");
        assert_eq!(pieces.bottom_right, "Phone BR");
        assert_eq!(pieces.bottom, "Phone Base");
        assert_eq!(pieces.bottom_left, "Phone BL");
        assert_eq!(pieces.left, "Phone Left");
        assert_eq!(sizing, Insets { left: 18.0, top: 18.0, right: 18.0, bottom: 18.0 });
        // No composite is fine; a piece or a side missing: no frame.
        let (_, _, composite) =
            parse_chrome_json(&PHONE11.replace("\"composite\"", "\"x\"")).unwrap();
        assert_eq!(composite, None);
        assert!(parse_chrome_json(&PHONE11.replace("\"topLeft\"", "\"x\"")).is_none());
        assert!(parse_chrome_json(&PHONE11.replace("\"leftWidth\"", "\"x\"")).is_none());
        assert!(parse_chrome_json("not json").is_none());
    }

    #[test]
    fn profile_names_chrome_and_mask() {
        assert_eq!(
            parse_profile(PROFILE),
            Some(Profile {
                chrome: Some("com.apple.dt.devicekit.chrome.phone11".into()),
                mask: Some("4E5532ED-1470-47D1-BDF4-7AA90C26957A".into()),
            })
        );
        assert_eq!(parse_screen_points(CAPABILITIES), Some((402.0, 874.0)));
        // A profile without either key still parses.
        assert_eq!(parse_profile("{}"), Some(Profile { chrome: None, mask: None }));
        assert_eq!(parse_screen_points("{}"), None);
    }

    /// A device type bundle and a chrome root in `dir`, laid out as Xcode
    /// lays them out, with the plists already as JSON. `chrome`: whether the
    /// named chrome is installed.
    fn install(dir: &Path, chrome: bool) -> (PathBuf, PathBuf) {
        let bundle = dir.join("iPhone 18 Pro.simdevicetype");
        let res = bundle.join("Contents/Resources");
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(res.join("profile.plist"), PROFILE).unwrap();
        std::fs::write(res.join("capabilities.plist"), CAPABILITIES).unwrap();
        std::fs::write(res.join("4E5532ED-1470-47D1-BDF4-7AA90C26957A.pdf"), b"").unwrap();
        let root = dir.join("Chrome");
        if chrome {
            let art = root.join("phone11.devicechrome/Contents/Resources");
            std::fs::create_dir_all(&art).unwrap();
            std::fs::write(art.join("chrome.json"), PHONE11).unwrap();
        }
        (bundle, root)
    }

    fn read(path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    #[test]
    fn a_device_type_gets_its_frame_mask_and_screen() {
        let dir = tempfile::tempdir().unwrap();
        let (bundle, root) = install(dir.path(), true);
        let spec = spec_from(&bundle, &root, read).unwrap();
        assert_eq!(spec.screen_pt, Some((402.0, 874.0)));
        assert_eq!(
            spec.mask.as_deref(),
            Some(bundle.join("Contents/Resources/4E5532ED-1470-47D1-BDF4-7AA90C26957A.pdf").as_path())
        );
        let frame = spec.frame.unwrap();
        assert_eq!(frame.dir, root.join("phone11.devicechrome/Contents/Resources"));
        assert_eq!(frame.pieces.left, "Phone Left");
        assert_eq!(frame.sizing.top, 18.0);
    }

    #[test]
    fn missing_chrome_falls_back_to_rounded_screen() {
        let dir = tempfile::tempdir().unwrap();
        let (bundle, root) = install(dir.path(), false);
        let spec = spec_from(&bundle, &root, read).unwrap();
        // No frame, but the mask still rounds the screen.
        assert!(spec.frame.is_none());
        assert!(spec.mask.is_some());
        // A mask the profile names but the bundle lacks is no mask.
        std::fs::remove_file(bundle.join("Contents/Resources/4E5532ED-1470-47D1-BDF4-7AA90C26957A.pdf"))
            .unwrap();
        let spec = spec_from(&bundle, &root, read).unwrap();
        assert!(spec.mask.is_none());
        // No profile at all: nothing to draw with.
        assert!(spec_from(&dir.path().join("gone"), &root, read).is_none());
    }

    #[test]
    fn without_a_screen_size_there_is_no_frame() {
        let dir = tempfile::tempdir().unwrap();
        let (bundle, root) = install(dir.path(), true);
        std::fs::write(bundle.join("Contents/Resources/capabilities.plist"), "{}").unwrap();
        let spec = spec_from(&bundle, &root, read).unwrap();
        assert!(spec.frame.is_none());
        assert!(spec.mask.is_some());
    }

    /// The Duo's chrome (phone15): the one with uneven sides.
    fn duo() -> FrameGeometry {
        FrameGeometry {
            screen_pt: (466.0, 678.0),
            sizing: Insets { left: 26.0, top: 14.0, right: 22.0, bottom: 14.0 },
        }
    }

    fn near(a: Rect, b: Rect) -> bool {
        [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)].iter().all(|(p, q)| (p - q).abs() < 0.01)
    }

    #[test]
    fn a_composite_stands_in_only_for_a_frame_its_size() {
        // phone11's composite for the iPhone 18 Pro: two points short.
        assert!(composite_fits((436.0, 908.0), (438.0, 910.0)));
        // phone15's for the Duo: exact.
        assert!(composite_fits((514.0, 706.0), (514.0, 706.0)));
        // phone12's (made for the 18 Pro Max) under the Air's smaller screen.
        assert!(!composite_fits((474.0, 990.0), (456.0, 948.0)));
    }

    #[test]
    fn insets_turn_with_the_device() {
        let i = duo().sizing;
        assert_eq!(i.turned(Quarter::None), i);
        // Clockwise: the left side goes to the top.
        assert_eq!(i.turned(Quarter::Cw90), Insets { left: 14.0, top: 26.0, right: 14.0, bottom: 22.0 });
        assert_eq!(i.turned(Quarter::Cw180), Insets { left: 22.0, top: 14.0, right: 26.0, bottom: 14.0 });
        assert_eq!(i.turned(Quarter::Cw270), Insets { left: 14.0, top: 22.0, right: 14.0, bottom: 26.0 });
    }

    #[test]
    fn the_screen_sits_inside_the_frame_at_its_insets() {
        // A frame drawn at its size in points: the screen is at the insets.
        let g = duo();
        let frame = Rect { x: 10.0, y: 20.0, w: 466.0 + 48.0, h: 678.0 + 28.0 };
        let screen = screen_rect_in_frame(frame, g, Quarter::None);
        assert!(near(screen, Rect { x: 36.0, y: 34.0, w: 466.0, h: 678.0 }), "{screen:?}");
        // Half size: everything halves.
        let half = Rect { x: 0.0, y: 0.0, w: frame.w / 2.0, h: frame.h / 2.0 };
        let screen = screen_rect_in_frame(half, g, Quarter::None);
        assert!(near(screen, Rect { x: 13.0, y: 7.0, w: 233.0, h: 339.0 }), "{screen:?}");
        // Turned clockwise: landscape, with the left side's 26 on top.
        let turned = Rect { x: 0.0, y: 0.0, w: 678.0 + 28.0, h: 466.0 + 48.0 };
        let screen = screen_rect_in_frame(turned, g, Quarter::Cw90);
        assert!(near(screen, Rect { x: 14.0, y: 26.0, w: 678.0, h: 466.0 }), "{screen:?}");
    }

    #[test]
    fn the_frame_is_fitted_and_centred_in_the_box() {
        let g = FrameGeometry {
            screen_pt: (402.0, 874.0),
            sizing: Insets { left: 18.0, top: 18.0, right: 18.0, bottom: 18.0 },
        };
        // A box twice the frame's height and exactly its width.
        let container = Rect { x: 0.0, y: 0.0, w: 438.0, h: 1820.0 };
        let placed = place(container, (1206.0, 2622.0), Some(g), Quarter::None);
        let frame = placed.frame.unwrap();
        assert!(near(frame, Rect { x: 0.0, y: 455.0, w: 438.0, h: 910.0 }), "{frame:?}");
        assert!(near(placed.screen, Rect { x: 18.0, y: 473.0, w: 402.0, h: 874.0 }), "{:?}", placed.screen);
        assert_eq!(placed.screen, screen_rect_in_frame(frame, g, Quarter::None));
        // Turned: the frame is landscape too.
        let wide = Rect { x: 0.0, y: 0.0, w: 1820.0, h: 438.0 };
        let placed = place(wide, (2622.0, 1206.0), Some(g), Quarter::Cw270);
        assert!(near(placed.frame.unwrap(), Rect { x: 455.0, y: 0.0, w: 910.0, h: 438.0 }));
    }

    #[test]
    fn without_a_frame_the_picture_is_fitted_alone() {
        let container = Rect { x: 5.0, y: 0.0, w: 100.0, h: 100.0 };
        let placed = place(container, (50.0, 100.0), None, Quarter::None);
        assert_eq!(placed.frame, None);
        assert!(near(placed.screen, Rect { x: 30.0, y: 0.0, w: 50.0, h: 100.0 }), "{:?}", placed.screen);
        // Nothing to fit yet: an empty rect, not a NaN.
        let placed = place(container, (0.0, 0.0), None, Quarter::None);
        assert_eq!(placed.screen.w, 0.0);
    }

    #[test]
    fn device_types_map_to_their_bundles() {
        let json = r#"{"devicetypes": [
            {"bundlePath": "/L/iPhone 18 Pro.simdevicetype",
             "identifier": "com.apple.CoreSimulator.SimDeviceType.iPhone-18-Pro", "name": "iPhone 18 Pro"},
            {"identifier": "com.apple.CoreSimulator.SimDeviceType.no-path"}
        ]}"#;
        let map = parse_device_type_bundles(json).unwrap();
        assert_eq!(map.get("iPhone-18-Pro"), Some(&PathBuf::from("/L/iPhone 18 Pro.simdevicetype")));
        assert_eq!(map.len(), 1);
        assert!(parse_device_type_bundles("nope").is_none());
    }

    /// Manual, to look at the result (needs Xcode's iPhone 18 Pro):
    /// `OO_FRAME_PNG_DIR=<dir> cargo test draws_the_frame -- --ignored`.
    /// Writes the overlay over a stand-in screen (a gradient) on a grey
    /// panel, framed and not, portrait and turned, as PNGs. With
    /// `OO_FRAME_SHOT=<png>` (an upright portrait `simctl io screenshot`)
    /// that is the screen instead, and `OO_FRAME_TYPE` names another device
    /// type (`iPad-Air-11-inch-M4`, say).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn draws_the_frame_for_a_look() {
        let dir = PathBuf::from(std::env::var("OO_FRAME_PNG_DIR").expect("set OO_FRAME_PNG_DIR"));
        let device_type = std::env::var("OO_FRAME_TYPE").unwrap_or_else(|_| "iPhone-18-Pro".into());
        let spec = chrome_for(&device_type).expect("that device type");
        let geometry = spec.geometry().expect("a frame");
        let panel = [200u8, 204, 210, 255];
        let shot = std::env::var_os("OO_FRAME_SHOT")
            .map(|p| image::open(p).expect("a screenshot").to_rgba8());
        for (name, turn, framed) in [
            ("framed", Quarter::None, true),
            ("framed-cw90", Quarter::Cw90, true),
            ("bare", Quarter::None, false),
            ("bare-cw270", Quarter::Cw270, false),
        ] {
            let scale = 2.0;
            let (fw, fh) = geometry.frame_pt(turn);
            let (sw, sh) = turned_size(geometry.screen_pt, turn);
            let (w, h) = if framed { (fw, fh) } else { (sw, sh) };
            let (w, h) = ((w * scale).round() as u32, (h * scale).round() as u32);
            let key = OverlayKey {
                device_type: device_type.clone(),
                size_px: (w, h),
                turn,
                framed,
                fill: panel,
            };
            let overlay = render::overlay(&spec, &key).expect("an overlay");
            assert_eq!(overlay.dimensions(), (w, h));
            let whole = Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 };
            let screen = if framed { screen_rect_in_frame(whole, geometry, turn) } else { whole };
            let mut out = image::RgbaImage::new(w, h);
            for (x, y, px) in out.enumerate_pixels_mut() {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let inside = fx >= screen.x && fx < screen.x + screen.w && fy >= screen.y && fy < screen.y + screen.h;
                let under = if inside {
                    let u = (fx - screen.x) / screen.w;
                    let v = (fy - screen.y) / screen.h;
                    match &shot {
                        Some(shot) if turn == Quarter::None => {
                            let sx = ((u * shot.width() as f32) as u32).min(shot.width() - 1);
                            let sy = ((v * shot.height() as f32) as u32).min(shot.height() - 1);
                            shot.get_pixel(sx, sy).0
                        }
                        _ => [(255.0 * u) as u8, (255.0 * v) as u8, 200, 255],
                    }
                } else {
                    panel
                };
                // The overlay is BGRA with straight alpha.
                let o = overlay.get_pixel(x, y).0;
                let a = f32::from(o[3]) / 255.0;
                let mix = |top: u8, bottom: u8| (f32::from(top) * a + f32::from(bottom) * (1.0 - a)) as u8;
                *px = image::Rgba([mix(o[2], under[0]), mix(o[1], under[1]), mix(o[0], under[2]), 255]);
            }
            out.save(dir.join(format!("{name}.png"))).unwrap();
        }
    }
}
