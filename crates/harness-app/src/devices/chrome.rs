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
//!   thickness against the picture; and `capabilities.DeviceCornerRadius`
//!   (62), the screen's corner radius in points, which the focus ring
//!   follows (it matches the mask: phone11's is opaque from 55 px along its
//!   diagonal at 3×, a 62.5 pt circle). The mask PDF's own size is no unit (it
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
//! drawn at their size and each edge's cross-section repeated between them,
//! on shared whole-pixel boundaries, which fits any screen without seams.
//!
//! gpui can't clip a surface to an image mask, so the frame is one overlay
//! drawn over the picture: the frame with the mask's shape cut out of it, or
//! with the frame off, the panel's colour with the same hole (it covers the
//! picture's corners). Parsing and layout are all-platform; rendering
//! (CoreGraphics) is macOS only.
//!
//! An Android emulator's frame comes from its AVD's skin instead
//! (`devices::android::skin`, [`FrameArt::Skin`]): a background image round
//! the screen and a foreground drawn over it, laid out in the skin's pixels
//! as a chrome is in points, through the same layout, overlay and cache. Its
//! screen's shape is a corner radius ([`ScreenMask::Rounded`]), which also
//! rounds an AVD with no skin.

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

/// A device's frame art, and how far it reaches past the screen.
// Read only by the macOS renderer; Android skins are only read on macOS.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FrameArt {
    /// A simulator's DeviceKit chrome: where its PDFs are and which they
    /// are. Its units are points.
    DeviceKit {
        dir: PathBuf,
        pieces: NineSlice,
        /// `images.composite`, the whole frame in one PDF, if the chrome has
        /// one.
        composite: Option<String>,
        sizing: Insets,
    },
    /// An Android emulator's skin (`devices::android::skin`). Its units are
    /// the skin's pixels, the display's own.
    Skin {
        /// The background image (`back.webp`), drawn at its own size.
        back: PathBuf,
        /// Where the background's top left is, from the frame's top left.
        back_at: (f32, f32),
        /// The foreground (`mask.webp`), the display's size: drawn over the
        /// picture (the screen's corners, the camera's cutout), which shows
        /// only where it is clear.
        foreground: Option<PathBuf>,
        /// The display's corner radius the skin gives. Without one the art
        /// alone shapes the framed screen: most skins' foreground rounds
        /// only the corners' last few pixels, and the older ones' screens
        /// are square.
        corner: Option<f32>,
        /// The radius of the screen's corners as the frame shows them: the
        /// larger of `corner` (0 without one) and the curve the foreground
        /// leaves clear. What the focus ring follows with the frame on.
        shown_corner: f32,
        sizing: Insets,
    },
}

impl FrameArt {
    /// How far the frame reaches past the screen on each side.
    pub(crate) fn sizing(&self) -> Insets {
        match self {
            FrameArt::DeviceKit { sizing, .. } | FrameArt::Skin { sizing, .. } => *sizing,
        }
    }
}

/// What cuts the screen's shape out of the overlay.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ScreenMask {
    /// A `framebufferMask` PDF, opaque where the screen shows, stretched over
    /// it.
    Pdf(PathBuf),
    /// A rectangle with corners of the spec's `screen_corner_pt` (square
    /// without one): an emulator's screen.
    // Made only for Android emulators, which are only found on macOS.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Rounded,
}

/// What the Device tab draws a device type with. A missing part leaves its
/// look out: no frame means the rounded screen alone, no mask too means the
/// picture as it comes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChromeSpec {
    /// The screen in points, portrait.
    pub screen_pt: Option<(f32, f32)>,
    /// Its corner radius, in points (0 for a square screen).
    pub screen_corner_pt: Option<f32>,
    /// The screen's shape: the `framebufferMask` PDF, or rounded corners.
    pub mask: Option<ScreenMask>,
    /// Only with `screen_pt`: the frame is laid out around it.
    pub frame: Option<FrameArt>,
}

impl ChromeSpec {
    /// The frame's layout, when there is a frame.
    pub(crate) fn geometry(&self) -> Option<FrameGeometry> {
        Some(FrameGeometry { screen_pt: self.screen_pt?, sizing: self.frame.as_ref()?.sizing() })
    }

    /// The corner radius the focus ring follows (`focus_ring`'s
    /// `corner_pt`): with a skin's frame on (`framed`), the screen's corners
    /// as its art shows them; otherwise the screen's own.
    pub(crate) fn ring_corner(&self, framed: bool) -> Option<f32> {
        match &self.frame {
            Some(FrameArt::Skin { shown_corner, .. }) if framed => Some(*shown_corner),
            _ => self.screen_corner_pt,
        }
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

/// `capabilities.plist` (as JSON): the screen's corner radius, in points.
pub(crate) fn parse_screen_corner(json: &str) -> Option<f32> {
    let root: Value = serde_json::from_str(json).ok()?;
    let radius = root.get("capabilities")?.get("DeviceCornerRadius")?.as_f64()?;
    (radius >= 0.0).then_some(radius as f32)
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
    let capabilities = read_plist(&res.join("capabilities.plist"));
    let screen_pt = capabilities.as_deref().and_then(parse_screen_points);
    let screen_corner_pt = capabilities.as_deref().and_then(parse_screen_corner);
    let mask = profile
        .mask
        .map(|name| res.join(format!("{name}.pdf")))
        .filter(|p| p.is_file())
        .map(ScreenMask::Pdf);
    let frame = profile.chrome.filter(|_| screen_pt.is_some()).and_then(|id| {
        // "com.apple.dt.devicekit.chrome.phone11" -> "phone11.devicechrome".
        let short = id.rsplit('.').next()?;
        let dir = chrome_root.join(format!("{short}.devicechrome/Contents/Resources"));
        let json = std::fs::read_to_string(dir.join("chrome.json")).ok()?;
        let (pieces, sizing, composite) = parse_chrome_json(&json)?;
        Some(FrameArt::DeviceKit { dir, pieces, composite, sizing })
    });
    Some(ChromeSpec { screen_pt, screen_corner_pt, mask, frame })
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

/// The focus ring round a screen painted at `picture` (upright, turned by
/// `turn` from the portrait `screen_pt`) inside `canvas`: its rect and
/// corner radius, `width` thick. It hugs the screen: just outside it where
/// the canvas has room (up to `width`), else on its edge, so it is never
/// clipped; its corners are the screen's (`corner_pt`, scaled to the
/// picture) grown by as much. An unknown screen size or corner is square.
pub(crate) fn focus_ring(
    picture: Rect,
    canvas: Rect,
    screen_pt: Option<(f32, f32)>,
    corner_pt: Option<f32>,
    turn: Quarter,
    width: f32,
) -> (Rect, f32) {
    let room = (picture.x - canvas.x)
        .min(picture.y - canvas.y)
        .min(canvas.x + canvas.w - (picture.x + picture.w))
        .min(canvas.y + canvas.h - (picture.y + picture.h));
    let out = room.clamp(0.0, width);
    let scale = screen_pt
        .map(|pt| turned_size(pt, turn).0)
        .filter(|w| *w > 0.0)
        .map_or(0.0, |w| picture.w / w);
    let corner = (corner_pt.unwrap_or(0.0) * scale).min(picture.w.min(picture.h) / 2.0);
    let ring = Rect {
        x: picture.x - out,
        y: picture.y - out,
        w: picture.w + 2.0 * out,
        h: picture.h + 2.0 * out,
    };
    (ring, corner + out)
}

/// How far, in points, a composite may differ from the frame it is drawn
/// over in either direction.
// Only the macOS renderer draws.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const COMPOSITE_SLACK_PT: f32 = 4.0;

/// Whether a composite of `composite` points can stand for a frame of
/// `frame` points (portrait both).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
    use objc2_core_foundation::{CFData, CFRetained, CFURL, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGContext,
        CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGInterpolationQuality,
        CGPDFBox, CGPDFDocument, CGPDFPage,
    };

    use super::{
        ChromeSpec, FrameArt, NineSlice, OverlayKey, PxRect, Quarter, Rect, ScreenMask, hole_px,
        turned_size,
    };

    /// A page of a PDF, and its size in points. The document is kept with
    /// its page, which it owns.
    struct Pdf {
        page: CFRetained<CGPDFPage>,
        _doc: CFRetained<CGPDFDocument>,
        bounds: CGRect,
    }

    fn open(path: &Path) -> Option<Pdf> {
        let url = CFURL::from_file_path(path)?;
        let doc = CGPDFDocument::with_url(Some(&url))?;
        let page = CGPDFDocument::page(Some(&doc), 1)?;
        let bounds = CGPDFPage::box_rect(Some(&page), CGPDFBox::MediaBox);
        (bounds.size.width > 0.0 && bounds.size.height > 0.0).then_some(Pdf { page, _doc: doc, bounds })
    }

    /// A blank BGRA bitmap, premultiplied (all CoreGraphics draws into),
    /// and its context drawing into `pixels`. The context is declared
    /// first so it is released first, before the pixels it draws into.
    struct Canvas {
        ctx: CFRetained<CGContext>,
        pixels: Vec<u8>,
        w: usize,
        h: usize,
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
            Some(Canvas { ctx, pixels, w, h })
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
            let Canvas { ctx, pixels, .. } = self;
            drop(ctx);
            pixels
        }

        /// Paint `line` (premultiplied BGRA pixels) over the pixels from
        /// (x, y), source over.
        fn blend(&mut self, x: usize, y: usize, line: &[u8]) {
            let at = (y * self.w + x) * 4;
            let Some(dst) = self.pixels.get_mut(at..at + line.len()) else {
                return;
            };
            for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(line.as_chunks::<4>().0) {
                let keep = 255 - u32::from(s[3]);
                for k in 0..4 {
                    d[k] = (u32::from(s[k]) + (u32::from(d[k]) * keep + 127) / 255).min(255) as u8;
                }
            }
        }
    }

    /// How long an edge piece is drawn before its middle line is taken.
    const STRIP: usize = 64;

    /// An edge piece's cross-section, `thick` pixels across: its line half
    /// way along, drawn `STRIP` long so art that stops short of the piece's
    /// ends (or blurs there) is left out. `along_y`: the piece runs down
    /// (Left, Right) rather than across (Top, Bottom).
    fn cross_section(pdf: &Pdf, thick: usize, along_y: bool) -> Option<Vec<u8>> {
        let (w, h) = if along_y { (thick, STRIP) } else { (STRIP, thick) };
        let strip = Canvas::new(w, h)?;
        strip.draw(pdf, Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 });
        let pixels = strip.into_pixels();
        let mid = STRIP / 2;
        Some(if along_y {
            pixels[mid * w * 4..(mid + 1) * w * 4].to_vec()
        } else {
            (0..h).flat_map(|y| pixels[(y * w + mid) * 4..(y * w + mid + 1) * 4].to_vec()).collect()
        })
    }

    /// The nine slices laid out on `canvas` (the whole frame), `s` pixels
    /// per point. Every boundary is a whole pixel shared by both pieces,
    /// with no overlap (two semi-transparent rims laid over each other show
    /// as a line). Each edge is its cross-section repeated exactly between
    /// the corners, and the corners are drawn at their size. So no join
    /// can leave a gap or a doubled row.
    fn draw_frame(canvas: &mut Canvas, dir: &Path, p: &NineSlice, s: f32) -> Option<()> {
        let piece = |name: &str| open(&dir.join(format!("{name}.pdf")));
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
        let (w, h) = (canvas.w, canvas.h);
        let px = |pt: f64| ((pt as f32 * s).round().max(1.0) as usize).min(w.min(h));
        let size = |pdf: &Pdf| (px(pdf.bounds.size.width), px(pdf.bounds.size.height));
        let ((tlw, tlh), (trw, trh)) = (size(&tl), size(&tr));
        let ((blw, blh), (brw, brh)) = (size(&bl), size(&br));
        // Left and right edges: rows, from the top corner's bottom to the
        // bottom corner's top.
        for (pdf, right) in [(&l, false), (&r, true)] {
            let thick = size(pdf).0;
            let line = cross_section(pdf, thick, true)?;
            let x = if right { w - thick } else { 0 };
            let (top, bottom) = if right { (trh, brh) } else { (tlh, blh) };
            for y in top..h.saturating_sub(bottom) {
                canvas.blend(x, y, &line);
            }
        }
        // Top and bottom edges: columns, the same way.
        for (pdf, bottom) in [(&t, false), (&b, true)] {
            let thick = size(pdf).1;
            let column = cross_section(pdf, thick, false)?;
            let y0 = if bottom { h - thick } else { 0 };
            let (left, right) = if bottom { (blw, brw) } else { (tlw, trw) };
            for x in left..w.saturating_sub(right) {
                for (k, px) in column.as_chunks::<4>().0.iter().enumerate() {
                    canvas.blend(x, y0 + k, px);
                }
            }
        }
        let rect = |x: usize, y: usize, w: usize, h: usize| Rect {
            x: x as f32,
            y: y as f32,
            w: w as f32,
            h: h as f32,
        };
        canvas.draw(&tl, rect(0, 0, tlw, tlh));
        canvas.draw(&tr, rect(w - trw, 0, trw, trh));
        canvas.draw(&bl, rect(0, h - blh, blw, blh));
        canvas.draw(&br, rect(w - brw, h - brh, brw, brh));
        Some(())
    }

    /// Cut the screen out of `pixels` (a `width` wide overlay) at `hole`:
    /// each pixel there is kept by `keep` (one alpha a pixel of the hole,
    /// row by row), or with no `keep` the whole rect goes.
    fn cut(pixels: &mut [u8], width: usize, hole: PxRect, keep: Option<&[u8]>) {
        let (x0, y0, x1, y1) = (hole.x0 as usize, hole.y0 as usize, hole.x1 as usize, hole.y1 as usize);
        let hw = x1 - x0;
        for y in 0..y1 - y0 {
            for x in 0..hw {
                let keep = keep.and_then(|k| k.get(y * hw + x)).map_or(0, |&k| u32::from(k));
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
    }

    /// How much of the overlay stays over each pixel of a `w` × `h` hole
    /// for `mask`: none where the screen shows. A PDF mask is opaque where
    /// the screen shows; rounded corners of `corner` pixels keep what is
    /// outside them. None: nothing stays.
    fn keep_for(mask: Option<&ScreenMask>, w: usize, h: usize, corner: f32) -> Option<Vec<u8>> {
        match mask? {
            ScreenMask::Pdf(path) => {
                let pdf = open(path)?;
                let m = Canvas::new(w, h)?;
                m.draw(&pdf, Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 });
                Some(m.into_pixels().as_chunks::<4>().0.iter().map(|px| 255 - px[3]).collect())
            }
            ScreenMask::Rounded => Some(super::outside_corners(w, h, corner)),
        }
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

    /// `art` (straight RGBA, as decoded) as a CGImage, over a copy of its
    /// pixels.
    fn cg_image(art: &RgbaImage) -> Option<CFRetained<CGImage>> {
        let (w, h) = (art.width() as usize, art.height() as usize);
        let data = CFData::from_bytes(art.as_raw());
        let provider = CGDataProvider::with_cf_data(Some(&data))?;
        let space = CGColorSpace::new_device_rgb()?;
        // SAFETY: the provider holds w × h × 4 bytes, RGBA with straight
        // alpha last, as described; a null decode array is allowed.
        unsafe {
            CGImage::new(
                w,
                h,
                8,
                32,
                w * 4,
                Some(&space),
                CGBitmapInfo(CGImageAlphaInfo::Last.0),
                Some(&provider),
                std::ptr::null(),
                true,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        }
    }

    impl Canvas {
        /// Draw `image` stretched over `r` (pixels, origin top left; a
        /// fraction of a pixel off the grid is fine: its edges land where
        /// `r` puts them).
        fn draw_image(&self, image: &CGImage, r: Rect) {
            if r.w <= 0.0 || r.h <= 0.0 {
                return;
            }
            let ctx = Some(&*self.ctx);
            CGContext::save_g_state(ctx);
            CGContext::set_interpolation_quality(ctx, CGInterpolationQuality::High);
            CGContext::draw_image(ctx, self.cg_rect(r), Some(image));
            CGContext::restore_g_state(ctx);
        }
    }

    /// A skin's frame: its background, anchored on the screen's hole so the
    /// art's screen edge is the picture's. Returns the foreground's
    /// premultiplied pixels, one pixel of the hole each (row by row), to
    /// composite after the background's rounded opening is cut.
    fn draw_skin(
        canvas: &mut Canvas,
        back: &Path,
        back_at: (f32, f32),
        foreground: Option<&Path>,
        geometry: super::FrameGeometry,
        hole: PxRect,
    ) -> Option<Option<Vec<u8>>> {
        let (sw, sh) = geometry.screen_pt;
        let (hw, hh) = hole.size();
        if sw <= 0.0 || sh <= 0.0 || hw == 0 || hh == 0 {
            return None;
        }
        let (sx, sy) = (hw as f32 / sw, hh as f32 / sh);
        let art = crate::devices::android::skin::art(back)?;
        let i = geometry.sizing;
        let at = Rect {
            x: hole.x0 as f32 + (back_at.0 - i.left) * sx,
            y: hole.y0 as f32 + (back_at.1 - i.top) * sy,
            w: art.width() as f32 * sx,
            h: art.height() as f32 * sy,
        };
        canvas.draw_image(&*cg_image(&art)?, at);
        let Some(path) = foreground else {
            return Some(None);
        };
        // Drawn on its own first: compositing it before the rounded cut
        // would retain the underlay's colour through a translucent edge.
        let (hw, hh) = (hw as usize, hh as usize);
        let fg = Canvas::new(hw, hh)?;
        let whole = Rect { x: 0.0, y: 0.0, w: hw as f32, h: hh as f32 };
        // The foreground is optional decoration: losing it must not throw
        // away the background already drawn.
        let Some(art) = crate::devices::android::skin::art(path) else {
            return Some(None);
        };
        let Some(image) = cg_image(&art) else {
            return Some(None);
        };
        fg.draw_image(&image, whole);
        Some(Some(fg.into_pixels()))
    }

    pub(crate) fn overlay(spec: &ChromeSpec, key: &OverlayKey) -> Option<RgbaImage> {
        let (pw, ph) = turned_size((key.size_px.0 as f32, key.size_px.1 as f32), key.turn);
        let (pw, ph) = (pw as usize, ph as usize);
        if pw == 0 || ph == 0 {
            return None;
        }
        let mut canvas = Canvas::new(pw, ph)?;
        let whole = Rect { x: 0.0, y: 0.0, w: pw as f32, h: ph as f32 };
        // A skin foreground goes over the background only after its rounded
        // opening is cut, preserving the foreground's own transparency.
        let mut foreground_pixels: Option<Vec<u8>> = None;
        // A framed skin's own corner radius, in place of the spec's (none
        // for a skin that gives none).
        let mut skin_corner: Option<Option<f32>> = None;
        let hole = if key.framed {
            let art = spec.frame.as_ref()?;
            let geometry = spec.geometry()?;
            let screen = hole_px((pw as u32, ph as u32), geometry, Quarter::None);
            // The screen's square corners reach past the frame's rounded
            // body: the panel's colour covers them there, under the frame
            // (a pixel wider all round, under the frame or on the panel).
            let under = Rect {
                x: (screen.x0 - 1).max(0) as f32,
                y: (screen.y0 - 1).max(0) as f32,
                w: (screen.x1 - screen.x0 + 2) as f32,
                h: (screen.y1 - screen.y0 + 2) as f32,
            };
            canvas.fill(under, key.fill);
            let frame_pt = geometry.frame_pt(Quarter::None);
            match art {
                FrameArt::DeviceKit { dir, pieces, composite, .. } => {
                    let composite = composite
                        .as_ref()
                        .and_then(|name| open(&dir.join(format!("{name}.pdf"))))
                        .filter(|pdf| {
                            let size = (pdf.bounds.size.width as f32, pdf.bounds.size.height as f32);
                            super::composite_fits(size, frame_pt)
                        });
                    match composite {
                        Some(pdf) => canvas.draw(&pdf, whole),
                        None => draw_frame(&mut canvas, dir, pieces, pw as f32 / frame_pt.0)?,
                    }
                }
                FrameArt::Skin { back, back_at, foreground, corner, .. } => {
                    skin_corner = Some(*corner);
                    foreground_pixels =
                        draw_skin(&mut canvas, back, *back_at, foreground.as_deref(), geometry, screen)?;
                }
            }
            screen
        } else {
            spec.mask.as_ref()?;
            canvas.fill(whole, key.fill);
            PxRect { x0: 0, y0: 0, x1: pw as i64, y1: ph as i64 }
        };
        let (hw, hh) = hole.size();
        // The screen's corners, scaled from points to the hole's pixels.
        let corner = match (spec.screen_pt, skin_corner.unwrap_or(spec.screen_corner_pt)) {
            (Some((sw, _)), Some(r)) if sw > 0.0 => r * hw as f32 / sw,
            _ => 0.0,
        };
        let keep = match &spec.mask {
            Some(mask) => Some(keep_for(Some(mask), hw as usize, hh as usize, corner)?),
            None => None,
        };
        cut(&mut canvas.pixels, pw, hole, keep.as_deref());
        if let Some(foreground) = foreground_pixels {
            let (x0, y0) = (hole.x0 as usize, hole.y0 as usize);
            for (y, row) in foreground.chunks_exact(hw as usize * 4).enumerate() {
                canvas.blend(x0, y0 + y, row);
            }
        }
        let mut pixels = canvas.into_pixels();
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

/// For a `w` × `h` screen with corners of `corner` pixels, how much of what
/// covers each pixel stays (row by row): all of it outside the rounded
/// corners, none inside, and the coverage on their edge.
// Only the macOS renderer cuts.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn outside_corners(w: usize, h: usize, corner: f32) -> Vec<u8> {
    let r = corner.min(w as f32 / 2.0).min(h as f32 / 2.0).max(0.0);
    let mut keep = vec![0u8; w * h];
    if r <= 0.0 {
        return keep;
    }
    let (fw, fh) = (w as f32, h as f32);
    for y in 0..h {
        let cy = y as f32 + 0.5;
        // The corner circles' centres, and how far into a corner this row is.
        let dy = if cy < r { r - cy } else if cy > fh - r { cy - (fh - r) } else { continue };
        for x in 0..w {
            let cx = x as f32 + 0.5;
            let dx = if cx < r { r - cx } else if cx > fw - r { cx - (fw - r) } else { continue };
            // How much of the pixel is inside the curve, about.
            let inside = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            keep[y * w + x] = ((1.0 - inside) * 255.0).round() as u8;
        }
    }
    keep
}

/// gpui's rounding of a logical edge to device pixels (its
/// `round_half_toward_zero`), so what is drawn here lands where gpui puts it.
pub(crate) fn device_px(logical: f32, scale: f32) -> i64 {
    let x = logical * scale;
    (x.abs() - 0.5).ceil().copysign(x) as i64
}

/// A rectangle of whole device pixels, [x0, x1) × [y0, y1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PxRect {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl PxRect {
    pub(crate) fn size(self) -> (u32, u32) {
        ((self.x1 - self.x0).max(0) as u32, (self.y1 - self.y0).max(0) as u32)
    }
}

/// Where gpui draws `r`: each edge snapped on its own.
pub(crate) fn snap(r: Rect, scale: f32) -> PxRect {
    let (x0, y0) = (device_px(r.x, scale), device_px(r.y, scale));
    PxRect {
        x0,
        y0,
        x1: device_px(r.x + r.w, scale).max(x0),
        y1: device_px(r.y + r.h, scale).max(y0),
    }
}

/// What is painted where, in logical pixels: the picture, and the overlay
/// with its size in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Painted {
    pub picture: Rect,
    pub overlay: Rect,
    pub overlay_px: (u32, u32),
}

/// Lay `placement` out for painting at `scale`, on gpui's device-pixel
/// grid, so the picture fills exactly the overlay's hole. The overlay is
/// drawn at its own size (its snapped rect), and the picture at the hole
/// inside it, both from the same whole numbers; laid out separately, they
/// could miss by a pixel and leave a picture column showing past the
/// frame's rounded corner.
pub(crate) fn painted(
    placement: Placement,
    geometry: Option<FrameGeometry>,
    turn: Quarter,
    scale: f32,
) -> Painted {
    let logical = |r: PxRect| Rect {
        x: r.x0 as f32 / scale,
        y: r.y0 as f32 / scale,
        w: (r.x1 - r.x0) as f32 / scale,
        h: (r.y1 - r.y0) as f32 / scale,
    };
    match (placement.frame, geometry) {
        (Some(frame), Some(geometry)) if scale > 0.0 => {
            let od = snap(frame, scale);
            let overlay_px = od.size();
            let hole = hole_px(overlay_px, geometry, turn);
            let picture = PxRect {
                x0: od.x0 + hole.x0,
                y0: od.y0 + hole.y0,
                x1: od.x0 + hole.x1,
                y1: od.y0 + hole.y1,
            };
            Painted { picture: logical(picture), overlay: logical(od), overlay_px }
        }
        _ => {
            let sd = snap(placement.screen, scale.max(f32::MIN_POSITIVE));
            let at = logical(sd);
            Painted { picture: at, overlay: at, overlay_px: sd.size() }
        }
    }
}

/// The screen's hole in an overlay of `size_px` (turned by `turn`), in its
/// pixels. The renderer works out the hole portrait and turns the image,
/// so this does the same: one source for both.
pub(crate) fn hole_px(size_px: (u32, u32), geometry: FrameGeometry, turn: Quarter) -> PxRect {
    let (pw, ph) = match turn {
        Quarter::Cw90 | Quarter::Cw270 => (size_px.1 as i64, size_px.0 as i64),
        Quarter::None | Quarter::Cw180 => (size_px.0 as i64, size_px.1 as i64),
    };
    let whole = Rect { x: 0.0, y: 0.0, w: pw as f32, h: ph as f32 };
    let r = screen_rect_in_frame(whole, geometry, Quarter::None);
    let edge = |v: f32, max: i64| (v.round() as i64).clamp(0, max);
    let (x0, y0) = (edge(r.x, pw), edge(r.y, ph));
    let (x1, y1) = (edge(r.x + r.w, pw).max(x0), edge(r.y + r.h, ph).max(y0));
    // Where `image::imageops::rotate*` moves that rect.
    match turn {
        Quarter::None => PxRect { x0, y0, x1, y1 },
        Quarter::Cw90 => PxRect { x0: ph - y1, y0: x0, x1: ph - y0, y1: x1 },
        Quarter::Cw180 => PxRect { x0: pw - x1, y0: ph - y1, x1: pw - x0, y1: ph - y0 },
        Quarter::Cw270 => PxRect { x0: y0, y0: pw - x1, x1: y1, y1: pw - x0 },
    }
}

/// The frame around a screen drawn at `screen` (turned by `turn`): the
/// inverse of [`screen_rect_in_frame`].
pub(crate) fn frame_around(screen: Rect, geometry: FrameGeometry, turn: Quarter) -> Rect {
    let (sw, _) = turned_size(geometry.screen_pt, turn);
    let (fw, fh) = geometry.frame_pt(turn);
    let i = geometry.sizing.turned(turn);
    let s = if sw > 0.0 { screen.w / sw } else { 0.0 };
    Rect { x: screen.x - i.left * s, y: screen.y - i.top * s, w: fw * s, h: fh * s }
}

/// The overlays drawn for the device shown: one per (turn, framed), so
/// turning the device back or toggling the frame again is a hit. All are
/// for one device type and panel colour; a new one of either clears the
/// rest.
#[derive(Clone)]
pub(crate) struct OverlayCache<T> {
    entries: Vec<(OverlayKey, T)>,
}

impl<T> OverlayCache<T> {
    pub(crate) fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub(crate) fn get(&self, key: &OverlayKey) -> Option<&T> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, image)| image)
    }

    /// Keep `image` for `key`. Returns what it pushes out (to free): the
    /// slot's older overlay (another size), or every one for another device
    /// type or colour.
    pub(crate) fn insert(&mut self, key: OverlayKey, image: T) -> Vec<T> {
        let (keep, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|(k, _)| {
                k.device_type == key.device_type
                    && k.fill == key.fill
                    && (k.turn, k.framed) != (key.turn, key.framed)
            });
        self.entries = keep;
        self.entries.push((key, image));
        gone.into_iter().map(|(_, image)| image).collect()
    }

    /// What to paint while `want` is drawn: it, or else the nearest kept,
    /// so the picture is never shown unclipped. Same turn first (the
    /// rounded corners sit right), then the same kind, then any; within
    /// each, the same device type and size first.
    pub(crate) fn closest(&self, want: &OverlayKey) -> Option<&(OverlayKey, T)> {
        self.entries.iter().min_by_key(|(k, _)| {
            (
                k.turn != want.turn,
                k.framed != want.framed,
                k.device_type != want.device_type,
                k.size_px != want.size_px,
            )
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
            "DeviceCornerRadius": 62,
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

    #[test]
    fn parses_the_screens_corner_radius() {
        // capabilities.DeviceCornerRadius, in points: 62 on the iPhone 18
        // Pro, 18 on an iPad, 0 (square) on an iPhone SE.
        assert_eq!(parse_screen_corner(CAPABILITIES), Some(62.0));
        let square = r#"{"capabilities": {"DeviceCornerRadius": 0}}"#;
        assert_eq!(parse_screen_corner(square), Some(0.0));
        assert_eq!(parse_screen_corner("{}"), None);
        assert_eq!(parse_screen_corner(r#"{"capabilities": {"DeviceCornerRadius": -3}}"#), None);
    }

    #[test]
    fn the_focus_ring_hugs_the_screen() {
        let r = |x, y, w, h| Rect { x, y, w, h };
        let screen = Some((402.0, 874.0));
        let corner = Some(62.0);
        // Frame on: room round the screen, so the ring sits just outside it,
        // its corners the screen's (62 pt at half size, 31) plus the ring.
        let canvas = r(0.0, 0.0, 300.0, 600.0);
        let framed = r(40.0, 60.0, 201.0, 437.0);
        assert_eq!(
            focus_ring(framed, canvas, screen, corner, Quarter::None, 2.0),
            (r(38.0, 60.0 - 2.0, 205.0, 441.0), 33.0)
        );
        assert_eq!(focus_ring(framed, canvas, screen, corner, Quarter::Cw180, 2.0).1, 33.0);
        // Frame off: the screen fills the canvas's width, so the ring stays
        // on it rather than be clipped, with the screen's own corners.
        let bare_canvas = r(0.0, 0.0, 201.0, 500.0);
        let bare = r(0.0, 31.5, 201.0, 437.0);
        assert_eq!(focus_ring(bare, bare_canvas, screen, corner, Quarter::None, 2.0), (bare, 31.0));
        // Turned, the screen's long side is across: the same corners.
        let turned = r(40.0, 60.0, 437.0, 201.0);
        let wide = r(0.0, 0.0, 600.0, 300.0);
        for turn in [Quarter::Cw90, Quarter::Cw270] {
            assert_eq!(
                focus_ring(turned, wide, screen, corner, turn, 2.0),
                (r(38.0, 58.0, 441.0, 205.0), 33.0),
                "{turn:?}"
            );
            let touching = r(0.0, 49.5, 600.0, 201.0 * 600.0 / 437.0);
            let (ring, radius) = focus_ring(touching, wide, screen, corner, turn, 2.0);
            assert_eq!(ring, touching, "{turn:?}");
            assert!((radius - 62.0 * 600.0 / 874.0).abs() < 1e-3, "{turn:?} {radius}");
        }
        // Inside the canvas however little room: a gap of 1 gives 1.
        let snug = r(1.0, 1.0, 201.0, 437.0);
        let (ring, radius) = focus_ring(snug, r(0.0, 0.0, 203.0, 439.0), screen, corner, Quarter::None, 2.0);
        assert_eq!((ring, radius), (r(0.0, 0.0, 203.0, 439.0), 32.0));
        // A square screen, or one whose corners aren't known: square.
        assert_eq!(focus_ring(framed, canvas, screen, Some(0.0), Quarter::None, 2.0).1, 2.0);
        assert_eq!(focus_ring(framed, canvas, None, None, Quarter::None, 2.0).1, 2.0);
    }

    /// With an Android skin's frame on, the ring follows the screen's
    /// corners as the frame shows them (its art rounds them more than the
    /// skin's radius); frame off, the screen's own, which cut it. A
    /// simulator's are its screen's either way.
    #[test]
    fn the_focus_ring_follows_a_skins_shown_corners() {
        let r = |x, y, w, h| Rect { x, y, w, h };
        let skin = ChromeSpec {
            screen_pt: Some((1280.0, 2856.0)),
            screen_corner_pt: Some(109.0),
            mask: Some(ScreenMask::Rounded),
            frame: Some(FrameArt::Skin {
                back: "/s/back.webp".into(),
                back_at: (0.0, 0.0),
                foreground: Some("/s/mask.webp".into()),
                corner: Some(109.0),
                shown_corner: 171.0,
                sizing: Insets { left: 60.0, top: 61.0, right: 68.0, bottom: 57.0 },
            }),
        };
        assert_eq!(skin.ring_corner(true), Some(171.0));
        assert_eq!(skin.ring_corner(false), Some(109.0));
        // At a quarter of the display's size, framed: 171 / 4, plus the ring.
        let canvas = r(0.0, 0.0, 400.0, 800.0);
        let picture = r(20.0, 20.0, 320.0, 714.0);
        let ring = |framed| {
            let corner = skin.ring_corner(framed);
            focus_ring(picture, canvas, skin.screen_pt, corner, Quarter::None, 2.0).1
        };
        assert_eq!(ring(true), 171.0 / 4.0 + 2.0);
        assert_eq!(ring(false), 109.0 / 4.0 + 2.0);
        // A simulator: its screen's corners, framed or not.
        let mut sim = skin.clone();
        sim.screen_corner_pt = Some(62.0);
        sim.frame = Some(FrameArt::DeviceKit {
            dir: "/c".into(),
            pieces: NineSlice {
                top_left: String::new(),
                top: String::new(),
                top_right: String::new(),
                right: String::new(),
                bottom_right: String::new(),
                bottom: String::new(),
                bottom_left: String::new(),
                left: String::new(),
            },
            composite: None,
            sizing: Insets { left: 18.0, top: 18.0, right: 18.0, bottom: 18.0 },
        });
        assert_eq!(sim.ring_corner(true), Some(62.0));
        assert_eq!(sim.ring_corner(false), Some(62.0));
        // No spec'd corners: none.
        sim.screen_corner_pt = None;
        assert_eq!(sim.ring_corner(true), None);
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
        assert_eq!(spec.screen_corner_pt, Some(62.0));
        assert_eq!(
            spec.mask,
            Some(ScreenMask::Pdf(
                bundle.join("Contents/Resources/4E5532ED-1470-47D1-BDF4-7AA90C26957A.pdf")
            ))
        );
        let Some(FrameArt::DeviceKit { dir, pieces, sizing, .. }) = spec.frame else {
            panic!("a DeviceKit frame");
        };
        assert_eq!(dir, root.join("phone11.devicechrome/Contents/Resources"));
        assert_eq!(pieces.left, "Phone Left");
        assert_eq!(sizing.top, 18.0);
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
            let scale: f32 = std::env::var("OO_FRAME_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(2.0);
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
        // `OO_FRAME_PANEL=<logical width>`: also paint it the way the
        // Device tab does (layout, device-pixel snapping, a light picture
        // under the overlay) on a dark panel, at an awkward origin.
        if let Some(width) = std::env::var("OO_FRAME_PANEL").ok().and_then(|w| w.parse::<f32>().ok()) {
            let scale: f32 = std::env::var("OO_FRAME_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(2.0);
            let panel = [47u8, 51, 64, 255];
            let light = [248u8, 246, 255, 255];
            let (fw, fh) = geometry.frame_pt(Quarter::None);
            let container = Rect { x: 8.25, y: 3.4, w: width, h: width * fh / fw + 9.3 };
            let placement = place(container, (0.0, 0.0), Some(geometry), Quarter::None);
            let p = painted(placement, Some(geometry), Quarter::None, scale);
            let key = OverlayKey {
                device_type: device_type.clone(),
                size_px: p.overlay_px,
                turn: Quarter::None,
                framed: true,
                fill: panel,
            };
            let overlay = render::overlay(&spec, &key).expect("an overlay");
            let all = snap(container, scale);
            let (od, pd) = (snap(p.overlay, scale), snap(p.picture, scale));
            let (w, h) = ((all.x1 + 4) as u32, (all.y1 + 4) as u32);
            let mut out = image::RgbaImage::from_pixel(w, h, image::Rgba(panel));
            for (x, y, px) in out.enumerate_pixels_mut() {
                let (x, y) = (x as i64, y as i64);
                if x >= pd.x0 && x < pd.x1 && y >= pd.y0 && y < pd.y1 {
                    *px = image::Rgba(light);
                }
                if x >= od.x0 && x < od.x1 && y >= od.y0 && y < od.y1 {
                    let o = overlay.get_pixel((x - od.x0) as u32, (y - od.y0) as u32).0;
                    let a = f32::from(o[3]) / 255.0;
                    let under = px.0;
                    let mix = |top: u8, bottom: u8| (f32::from(top) * a + f32::from(bottom) * (1.0 - a)) as u8;
                    *px = image::Rgba([mix(o[2], under[0]), mix(o[1], under[1]), mix(o[0], under[2]), 255]);
                }
            }
            out.save(dir.join("panel.png")).unwrap();
        }
    }

    fn key(turn: Quarter, framed: bool, w: u32) -> OverlayKey {
        OverlayKey {
            device_type: "iPhone-18-Pro".into(),
            size_px: (w, w * 2),
            turn,
            framed,
            fill: [1, 2, 3, 255],
        }
    }

    #[test]
    fn the_frame_around_a_screen_is_the_inverse_of_the_screen_in_it() {
        let g = duo();
        for turn in [Quarter::None, Quarter::Cw90, Quarter::Cw180, Quarter::Cw270] {
            let (fw, fh) = g.frame_pt(turn);
            let frame = Rect { x: 7.0, y: 3.0, w: fw * 0.7, h: fh * 0.7 };
            let screen = screen_rect_in_frame(frame, g, turn);
            assert!(near(frame_around(screen, g, turn), frame), "{turn:?}");
        }
    }

    #[test]
    fn rotating_back_and_forth_hits_the_overlay_cache() {
        let mut cache = OverlayCache::new();
        for (turn, framed) in [
            (Quarter::None, true),
            (Quarter::Cw90, true),
            (Quarter::None, false),
            (Quarter::Cw90, false),
        ] {
            assert!(cache.insert(key(turn, framed, 100), (turn, framed)).is_empty());
        }
        // Each (turn, framed) is kept: back to portrait framed is a hit.
        assert_eq!(cache.get(&key(Quarter::None, true, 100)), Some(&(Quarter::None, true)));
        assert_eq!(cache.get(&key(Quarter::Cw90, false, 100)), Some(&(Quarter::Cw90, false)));
        // A new size replaces that slot's overlay only.
        assert_eq!(cache.insert(key(Quarter::None, true, 120), (Quarter::Cw180, true)), vec![(Quarter::None, true)]);
        assert_eq!(cache.get(&key(Quarter::None, true, 100)), None);
        assert!(cache.get(&key(Quarter::Cw90, true, 100)).is_some());
        // Another device type or panel colour: the old ones go.
        let other = OverlayKey { device_type: "iPhone-Air".into(), ..key(Quarter::None, true, 100) };
        assert_eq!(cache.insert(other.clone(), (Quarter::Cw270, false)).len(), 4);
        assert_eq!(cache.get(&other), Some(&(Quarter::Cw270, false)));
        let dark = OverlayKey { fill: [9, 9, 9, 255], ..other.clone() };
        assert_eq!(cache.insert(dark.clone(), (Quarter::None, false)).len(), 1);
    }

    #[test]
    fn until_its_overlay_lands_the_closest_one_is_painted() {
        let mut cache = OverlayCache::new();
        assert!(cache.closest(&key(Quarter::None, true, 100)).is_none());
        cache.insert(key(Quarter::None, false, 90), "bare");
        // Nothing framed yet: the rounded screen rather than nothing.
        assert_eq!(cache.closest(&key(Quarter::None, true, 100)).map(|e| e.1), Some("bare"));
        cache.insert(key(Quarter::Cw90, true, 100), "framed turned");
        // Same turn wins over same kind: its corners are in the right place.
        assert_eq!(cache.closest(&key(Quarter::None, true, 100)).map(|e| e.1), Some("bare"));
        cache.insert(key(Quarter::None, true, 80), "framed small");
        // Same turn and kind, another size: stretched, it beats the rest.
        assert_eq!(cache.closest(&key(Quarter::None, true, 100)).map(|e| e.1), Some("framed small"));
        // An exact hit is that one.
        assert_eq!(cache.closest(&key(Quarter::Cw90, true, 100)).map(|e| e.1), Some("framed turned"));
        // Only other turns: still something.
        let mut turned = OverlayCache::new();
        turned.insert(key(Quarter::Cw90, true, 100), "turned");
        assert_eq!(turned.closest(&key(Quarter::Cw270, false, 100)).map(|e| e.1), Some("turned"));
    }

    /// A one-page PDF of `w` × `h` points drawing `content`.
    #[cfg(target_os = "macos")]
    fn write_pdf(path: &Path, (w, h): (f32, f32), content: &str) {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents 4 0 R /Resources << >> >>"
            ),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (n, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out += &format!("{} 0 obj\n{object}\nendobj\n", n + 1);
        }
        let xref = out.len();
        out += &format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
        for offset in offsets {
            out += &format!("{offset:010} 00000 n \n");
        }
        out += &format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1);
        std::fs::write(path, out).unwrap();
    }

    /// Nine slices drawn at a few scales leave no gap at their joins, even
    /// when an edge piece's art stops a hair short of its ends (as art
    /// stretched hundreds of times along a 1-point piece can): the frame's
    /// band stays opaque all round.
    #[cfg(target_os = "macos")]
    #[test]
    fn slice_joins_leave_no_seam() {
        let dir = tempfile::tempdir().unwrap();
        let grey = "0.3 g";
        for corner in ["TL", "TR", "BL", "BR"] {
            write_pdf(&dir.path().join(format!("{corner}.pdf")), (110.0, 110.0), &format!("{grey} 0 0 110 110 re f"));
        }
        for side in ["Left", "Right"] {
            write_pdf(&dir.path().join(format!("{side}.pdf")), (110.0, 1.0), &format!("{grey} 0 0.05 110 0.9 re f"));
        }
        for side in ["Top", "Base"] {
            write_pdf(&dir.path().join(format!("{side}.pdf")), (1.0, 110.0), &format!("{grey} 0.05 0 0.9 110 re f"));
        }
        let name = |n: &str| n.to_string();
        let sizing = Insets { left: 18.0, top: 18.0, right: 18.0, bottom: 18.0 };
        let spec = ChromeSpec {
            screen_pt: Some((402.0, 874.0)),
            screen_corner_pt: Some(62.0),
            mask: None,
            frame: Some(FrameArt::DeviceKit {
                dir: dir.path().to_path_buf(),
                pieces: NineSlice {
                    top_left: name("TL"),
                    top: name("Top"),
                    top_right: name("TR"),
                    right: name("Right"),
                    bottom_right: name("BR"),
                    bottom: name("Base"),
                    bottom_left: name("BL"),
                    left: name("Left"),
                },
                composite: None,
                sizing,
            }),
        };
        let geometry = spec.geometry().unwrap();
        for scale in [0.8, 1.0, 1.37, 2.0, 3.0] {
            let (fw, fh) = geometry.frame_pt(Quarter::None);
            let (w, h) = ((fw * scale).round() as u32, (fh * scale).round() as u32);
            let key = OverlayKey {
                device_type: "x".into(),
                size_px: (w, h),
                turn: Quarter::None,
                framed: true,
                fill: [255, 0, 255, 255],
            };
            let image = render::overlay(&spec, &key).unwrap();
            let whole = Rect { x: 0.0, y: 0.0, w: w as f32, h: h as f32 };
            let hole = screen_rect_in_frame(whole, geometry, Quarter::None);
            let mut gaps = Vec::new();
            for (x, y, px) in image.enumerate_pixels() {
                // The band: outside the screen, a pixel clear of its edge.
                let (fx, fy) = (x as f32, y as f32);
                let near_hole = fx > hole.x - 2.0
                    && fx < hole.x + hole.w + 1.0
                    && fy > hole.y - 2.0
                    && fy < hole.y + hole.h + 1.0;
                if !near_hole && px.0[3] < 250 {
                    gaps.push((x, y, px.0[3]));
                }
            }
            assert!(gaps.is_empty(), "scale {scale}: {} gap pixels, e.g. {:?}", gaps.len(), &gaps[..gaps.len().min(5)]);
        }
    }

    /// A rounded rect filling `w` × `h` with corners of `r`, as PDF path ops.
    #[cfg(target_os = "macos")]
    fn rounded_rect(w: f32, h: f32, r: f32) -> String {
        let k = r * 0.5523;
        format!(
            "{r} 0 m {} 0 l {} 0 {w} {} {w} {r} c {w} {} l {w} {} {} {h} {} {h} c {r} {h} l {} {h} 0 {} 0 {} c \
             0 {r} l 0 {k2} {k2} 0 {r} 0 c f",
            w - r, w - r + k, r - k, h - r, h - r + k, w - r + k, w - r, r - k, h - r + k, h - r,
            k2 = r - k,
        )
    }

    /// Synthetic nine-slice art shaped like a phone (corners round off
    /// outside a 110-pt radius, so a screen's square corners reach past the
    /// body, as on the real ones) and a rounded mask, in `dir`.
    #[cfg(target_os = "macos")]
    fn phone_like(dir: &Path) -> ChromeSpec {
        let grey = "0.3 g";
        // TL as drawn in PDF space (y up): its body is the quarter disc
        // centred on its inner corner (110, 0).
        let k = 110.0 * 0.5523;
        let tl = format!("{grey} 0 0 m 110 0 l 110 110 l {} 110 0 {k} 0 0 c f", 110.0 - k);
        for (name, flip) in [
            ("TL", "1 0 0 1 0 0 cm"),
            ("TR", "-1 0 0 1 110 0 cm"),
            ("BL", "1 0 0 -1 0 110 cm"),
            ("BR", "-1 0 0 -1 110 110 cm"),
        ] {
            write_pdf(&dir.join(format!("{name}.pdf")), (110.0, 110.0), &format!("{flip} {tl}"));
        }
        for side in ["Left", "Right"] {
            write_pdf(&dir.join(format!("{side}.pdf")), (110.0, 1.0), &format!("{grey} 0 0 110 1 re f"));
        }
        for side in ["Top", "Base"] {
            write_pdf(&dir.join(format!("{side}.pdf")), (1.0, 110.0), &format!("{grey} 0 0 1 110 re f"));
        }
        write_pdf(&dir.join("mask.pdf"), (402.0, 874.0), &format!("0 g {}", rounded_rect(402.0, 874.0, 55.0)));
        let name = |n: &str| n.to_string();
        ChromeSpec {
            screen_pt: Some((402.0, 874.0)),
            screen_corner_pt: Some(62.0),
            mask: Some(ScreenMask::Pdf(dir.join("mask.pdf"))),
            frame: Some(FrameArt::DeviceKit {
                dir: dir.to_path_buf(),
                pieces: NineSlice {
                    top_left: name("TL"),
                    top: name("Top"),
                    top_right: name("TR"),
                    right: name("Right"),
                    bottom_right: name("BR"),
                    bottom: name("Base"),
                    bottom_left: name("BL"),
                    left: name("Left"),
                },
                composite: None,
                sizing: Insets { left: 18.0, top: 18.0, right: 18.0, bottom: 18.0 },
            }),
        }
    }

    /// The picture never shows outside the rounded screen: painted the way
    /// gpui paints (each rect snapped to device pixels, the overlay over
    /// the picture), every picture pixel is either in the overlay's hole or
    /// under an opaque overlay pixel. Run at the user's size (a frame about
    /// 712 px wide at 2×, and at 1.5×), at awkward origins, portrait and
    /// turned. The old layout left a picture column showing just past the
    /// frame's rounded corner: light ticks at the screen's sides, beside
    /// the corner regions.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_picture_never_shows_past_the_rounded_screen() {
        let dir = tempfile::tempdir().unwrap();
        let spec = phone_like(dir.path());
        let leaks = leaks(&spec, &[Quarter::None, Quarter::Cw90], SIZES, ORIGINS);
        assert!(leaks.is_empty(), "{} leaking pixels, e.g. {:?}", leaks.len(), &leaks[..leaks.len().min(6)]);
    }

    /// (Display scale, frame width) and panel origins to paint at: the
    /// user's size (a frame about 712 px wide at 2×, and at 1.5×), at
    /// awkward origins.
    #[cfg(target_os = "macos")]
    const SIZES: &[(f32, f32)] = &[(2.0, 356.0), (1.5, 356.0), (2.0, 357.3), (1.5, 349.7)];
    #[cfg(target_os = "macos")]
    const ORIGINS: &[(f32, f32)] = &[(0.0, 0.0), (8.25, 3.4), (11.3, 7.75), (4.6, 0.5)];

    /// The picture pixels `spec`'s framed overlay leaves showing outside
    /// its hole, painted the way gpui paints, at `sizes` and `origins`, in
    /// `turns`.
    #[cfg(target_os = "macos")]
    #[allow(clippy::type_complexity)]
    fn leaks(
        spec: &ChromeSpec,
        turns: &[Quarter],
        sizes: &[(f32, f32)],
        origins: &[(f32, f32)],
    ) -> Vec<(f32, f32, f32, f32, Quarter, i64, i64)> {
        let geometry = spec.geometry().unwrap();
        let mut leaks = Vec::new();
        for &(scale, width) in sizes {
            for &(ox, oy) in origins {
                for &turn in turns {
                    let (fw, fh) = geometry.frame_pt(turn);
                    let container = if matches!(turn, Quarter::None | Quarter::Cw180) {
                        Rect { x: ox, y: oy, w: width, h: width * fh / fw + 40.0 }
                    } else {
                        Rect { x: ox, y: oy, w: width * fw / fh, h: width + 0.37 }
                    };
                    let placement = place(container, (0.0, 0.0), Some(geometry), turn);
                    let p = painted(placement, Some(geometry), turn, scale);
                    let key = OverlayKey {
                        device_type: "x".into(),
                        size_px: p.overlay_px,
                        turn,
                        framed: true,
                        fill: [10, 20, 30, 255],
                    };
                    let overlay = render::overlay(spec, &key).unwrap();
                    let (od, pd) = (snap(p.overlay, scale), snap(p.picture, scale));
                    let hole = hole_px(p.overlay_px, geometry, turn);
                    let (ow, oh) = overlay.dimensions();
                    // Rectangular leak checks do not cover these points:
                    // they are inside hole_px but outside the rounded curve.
                    for (x, y) in [
                        (hole.x0, hole.y0),
                        (hole.x1 - 1, hole.y0),
                        (hole.x0, hole.y1 - 1),
                        (hole.x1 - 1, hole.y1 - 1),
                    ] {
                        // A soft art edge may be 254; a missed cut is clear.
                        assert!(
                            overlay.get_pixel(x as u32, y as u32).0[3] >= 254,
                            "uncovered rounded corner at scale {scale}, width {width}, origin {ox},{oy}, {turn:?}: {x},{y}"
                        );
                    }
                    for y in pd.y0..pd.y1 {
                        for x in pd.x0..pd.x1 {
                            // gpui stretches the sprite over its snapped rect.
                            let ix = ((x - od.x0) as f32 + 0.5) * ow as f32 / od.size().0 as f32;
                            let iy = ((y - od.y0) as f32 + 0.5) * oh as f32 / od.size().1 as f32;
                            let (ix, iy) = (ix as i64, iy as i64);
                            let in_hole = ix >= hole.x0 && ix < hole.x1 && iy >= hole.y0 && iy < hole.y1;
                            let covered = ix >= 0
                                && iy >= 0
                                && ix < ow as i64
                                && iy < oh as i64
                                && overlay.get_pixel(ix as u32, iy as u32).0[3] == 255;
                            if !in_hole && !covered {
                                leaks.push((scale, width, ox, oy, turn, x - pd.x0, y - pd.y0));
                            }
                        }
                    }
                }
            }
        }
        leaks
    }

    /// The Pixel 9 Pro's skin, as its `layout` lays it out: a 1280 × 2856
    /// display at (60, 61) in a 1408 × 2974 frame, corners of 109.
    #[cfg(target_os = "macos")]
    fn pixel_9_pro() -> FrameGeometry {
        FrameGeometry {
            screen_pt: (1280.0, 2856.0),
            sizing: Insets { left: 60.0, top: 61.0, right: 68.0, bottom: 57.0 },
        }
    }

    /// A stand-in skin at about a quarter of the Pixel 9 Pro's size (so
    /// its art is quick to encode and decode): a 320 × 714 display at
    /// (15, 15) in a 352 × 744 frame.
    #[cfg(target_os = "macos")]
    fn small_skin() -> FrameGeometry {
        FrameGeometry {
            screen_pt: (320.0, 714.0),
            sizing: Insets { left: 15.0, top: 15.0, right: 17.0, bottom: 15.0 },
        }
    }

    /// A blue camera dot at the display's top middle, in [`small_skin`]'s
    /// foreground.
    #[cfg(target_os = "macos")]
    fn camera(x: u32, y: u32) -> bool {
        let (dx, dy) = (x as f32 - 160.0, y as f32 - 22.5);
        dx * dx + dy * dy < 10.0 * 10.0
    }

    /// Stand-in art (not Google's) for [`small_skin`] in `dir`: a red body
    /// clear over the display, and a foreground opaque blue where `mask`
    /// says (none: no foreground). `corner`: the layout's radius, if any.
    #[cfg(target_os = "macos")]
    fn skin_like(dir: &Path, corner: Option<f32>, mask: Option<fn(u32, u32) -> bool>) -> ChromeSpec {
        let g = small_skin();
        let back = image::RgbaImage::from_fn(352, 744, |x, y| {
            let screen = (15..335).contains(&x) && (15..729).contains(&y);
            image::Rgba(if screen { [0, 0, 0, 0] } else { [200, 30, 40, 255] })
        });
        back.save_with_format(dir.join("back.webp"), image::ImageFormat::WebP).unwrap();
        let foreground = mask.map(|mask| {
            let art = image::RgbaImage::from_fn(320, 714, |x, y| {
                image::Rgba(if mask(x, y) { [0, 0, 255, 255] } else { [0, 0, 0, 0] })
            });
            art.save_with_format(dir.join("mask.webp"), image::ImageFormat::WebP).unwrap();
            dir.join("mask.webp")
        });
        ChromeSpec {
            screen_pt: Some(g.screen_pt),
            // Frame off: the skin's radius or the default.
            screen_corner_pt: Some(corner.unwrap_or(320.0 * 0.08)),
            mask: Some(ScreenMask::Rounded),
            frame: Some(FrameArt::Skin {
                back: dir.join("back.webp"),
                back_at: (0.0, 0.0),
                foreground,
                corner,
                shown_corner: corner.unwrap_or(0.0),
                sizing: g.sizing,
            }),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn android_overlay_hole_matches_the_display_rect() {
        use crate::devices::android::skin::{AndroidSkin, parse_layout, tests::PIXEL_9_PRO};
        let layout = parse_layout(PIXEL_9_PRO).expect("the Pixel 9 Pro's layout");
        let skin = AndroidSkin { layout, back: "/s/back.webp".into(), mask: None };
        let g = skin.spec().geometry().expect("a frame");
        assert_eq!(g, pixel_9_pro());
        // At the skin's own size, the hole is the display, in every turn
        // (where `rotate*` moves it).
        let at = |turn| {
            let (w, h) = turned_size((1408.0, 2974.0), turn);
            hole_px((w as u32, h as u32), g, turn)
        };
        assert_eq!(at(Quarter::None), PxRect { x0: 60, y0: 61, x1: 1340, y1: 2917 });
        assert_eq!(at(Quarter::Cw90), PxRect { x0: 57, y0: 60, x1: 2913, y1: 1340 });
        assert_eq!(at(Quarter::Cw180), PxRect { x0: 68, y0: 57, x1: 1348, y1: 2913 });
        assert_eq!(at(Quarter::Cw270), PxRect { x0: 61, y0: 68, x1: 2917, y1: 1348 });
        // Painted in a panel, the picture is exactly that hole of the
        // overlay as drawn, whichever way the emulator is turned.
        for scale in [1.0, 1.5, 2.0, 3.0] {
            for (ox, oy) in [(0.0, 0.0), (8.25, 3.4), (11.3, 7.75)] {
                for turn in [Quarter::None, Quarter::Cw90, Quarter::Cw180, Quarter::Cw270] {
                    let container = Rect { x: ox, y: oy, w: 357.3, h: 731.9 };
                    let p = painted(place(container, (0.0, 0.0), Some(g), turn), Some(g), turn, scale);
                    let od = snap(p.overlay, scale);
                    assert_eq!(od.size(), p.overlay_px, "{scale} {turn:?}");
                    let hole = hole_px(p.overlay_px, g, turn);
                    let at = PxRect { x0: od.x0 + hole.x0, y0: od.y0 + hole.y0, x1: od.x0 + hole.x1, y1: od.y0 + hole.y1 };
                    assert_eq!(snap(p.picture, scale), at, "{scale} {ox},{oy} {turn:?}");
                    // The display's share of the frame on each side.
                    let (fw, fh) = turned_size((p.overlay_px.0 as f32, p.overlay_px.1 as f32), turn);
                    let left = hole_px((fw as u32, fh as u32), g, Quarter::None).x0 as f32 / fw;
                    assert!((left - 60.0 / 1408.0).abs() < 1.0 / fw, "{scale} {turn:?} {left}");
                }
            }
        }
    }

    /// The skin's overlay: its art round the hole, the screen clear in
    /// it but for the foreground's camera and the rounded corners, in the
    /// right colours (BGRA out), turned with the emulator.
    #[cfg(target_os = "macos")]
    #[test]
    fn android_overlay_draws_the_skin_round_the_screen() {
        let dir = tempfile::tempdir().unwrap();
        let spec = skin_like(dir.path(), Some(109.0 / 4.0), Some(camera));
        let g = spec.geometry().unwrap();
        let fill = [10, 200, 30, 255];
        for turn in [Quarter::None, Quarter::Cw90, Quarter::Cw180, Quarter::Cw270] {
            // The skin's own size.
            let size_px = match turn {
                Quarter::None | Quarter::Cw180 => (352, 744),
                Quarter::Cw90 | Quarter::Cw270 => (744, 352),
            };
            let key = OverlayKey { device_type: "a".into(), size_px, turn, framed: true, fill };
            let overlay = render::overlay(&spec, &key).unwrap();
            assert_eq!(overlay.dimensions(), size_px);
            // Upright again, to look in the portrait layout.
            let upright = match turn {
                Quarter::None => overlay.clone(),
                Quarter::Cw90 => image::imageops::rotate270(&overlay),
                Quarter::Cw180 => image::imageops::rotate180(&overlay),
                Quarter::Cw270 => image::imageops::rotate90(&overlay),
            };
            let hole = hole_px((352, 744), g, Quarter::None);
            assert_eq!(hole, PxRect { x0: 15, y0: 15, x1: 335, y1: 729 }, "{turn:?}");
            let px = |x: i64, y: i64| upright.get_pixel(x as u32, y as u32).0;
            // The body, red, BGRA: 40, 30, 200.
            assert_eq!(px(5, 300), [40, 30, 200, 255], "{turn:?}");
            assert_eq!(px(hole.x1 + 2, 300), [40, 30, 200, 255], "{turn:?}");
            // The screen shows in the hole, right up to its edges.
            assert_eq!(px(175, 400)[3], 0, "{turn:?}");
            for (x, y) in [(hole.x0, 400), (hole.x1 - 1, 400), (175, hole.y0), (175, hole.y1 - 1)] {
                assert_eq!(px(x, y)[3], 0, "{turn:?} edge {x},{y}");
            }
            // The camera dot over it, blue (BGRA 255, 0, 0).
            let camera = px(hole.x0 + 160, hole.y0 + 22);
            assert_eq!(camera, [255, 0, 0, 255], "{turn:?}");
            // Its corners: covered (by the panel's colour where the art is
            // clear, and the art's soft edge), never the picture.
            assert_eq!(px(hole.x0, hole.y0)[3], 255, "{turn:?}");
            assert_eq!(px(hole.x1 - 1, hole.y1 - 1)[3], 255, "{turn:?}");
            assert_eq!(px(hole.x0 + 3, hole.y0 + 3), [30, 200, 10, 255], "{turn:?}");
        }
        // Nothing shows past the rounded screen, at any size or turn (a
        // few of each: the art takes a while to decode in a debug build).
        let all = [Quarter::None, Quarter::Cw90, Quarter::Cw180, Quarter::Cw270];
        let leaks = leaks(&spec, &all, &[SIZES[0], SIZES[3]], &[ORIGINS[1], ORIGINS[3]]);
        assert!(leaks.is_empty(), "{} leaking pixels, e.g. {:?}", leaks.len(), &leaks[..leaks.len().min(6)]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn skin_foreground_is_composited_after_the_screen_cutout() {
        let dir = tempfile::tempdir().unwrap();
        let spec = skin_like(dir.path(), Some(109.0 / 4.0), Some(camera));
        let art = image::RgbaImage::from_fn(320, 714, |x, y| {
            if (150..170).contains(&x) && (390..410).contains(&y) {
                image::Rgba([0, 0, 255, 128])
            } else if camera(x, y) {
                image::Rgba([0, 0, 255, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            }
        });
        art.save_with_format(dir.path().join("mask.webp"), image::ImageFormat::WebP).unwrap();
        std::fs::File::options()
            .write(true)
            .open(dir.path().join("mask.webp"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        let key = OverlayKey {
            device_type: "a".into(),
            size_px: (352, 744),
            turn: Quarter::None,
            framed: true,
            fill: [10, 200, 30, 255],
        };
        let overlay = render::overlay(&spec, &key).unwrap();
        // No green from the opaque panel underlay survives in the
        // translucent blue; the opaque camera still covers completely.
        assert_eq!(overlay.get_pixel(175, 410).0, [255, 0, 0, 128]);
        assert_eq!(overlay.get_pixel(175, 37).0, [255, 0, 0, 255]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_broken_optional_foreground_keeps_the_skin_background() {
        let dir = tempfile::tempdir().unwrap();
        let spec = skin_like(dir.path(), Some(109.0 / 4.0), Some(camera));
        std::fs::write(dir.path().join("mask.webp"), b"not webp").unwrap();
        let key = OverlayKey {
            device_type: "a".into(),
            size_px: (352, 744),
            turn: Quarter::None,
            framed: true,
            fill: [10, 200, 30, 255],
        };
        let overlay = render::overlay(&spec, &key).expect("the background without its foreground");
        assert_eq!(overlay.get_pixel(5, 300).0, [40, 30, 200, 255]);
        assert_eq!(overlay.get_pixel(175, 400).0[3], 0);
        assert_eq!(overlay.get_pixel(15, 15).0[3], 255);
    }

    /// A framed skin that gives no corner radius is shaped by its art
    /// alone: where its foreground is clear the screen shows, so the
    /// default radius never paints the panel's colour inside the bezel
    /// (pixel_6's foreground rounds only its corners' last few pixels).
    /// With no foreground either (nexus_5), the screen is square. Frame
    /// off, the default radius still rounds it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_framed_skin_without_a_radius_is_shaped_by_its_art() {
        // Opaque only in a small triangle at each corner, as pixel_6's.
        fn tips(x: u32, y: u32) -> bool {
            let (dx, dy) = (x.min(319 - x), y.min(713 - y));
            dx + dy < 3
        }
        let fill = [10, 200, 30, 255];
        let key = |framed| OverlayKey {
            device_type: "a".into(),
            size_px: if framed { (352, 744) } else { (320, 714) },
            turn: Quarter::None,
            framed,
            fill,
        };
        for (name, mask) in [("pixel_6", Some(tips as fn(u32, u32) -> bool)), ("nexus_5", None)] {
            let dir = tempfile::tempdir().unwrap();
            let spec = skin_like(dir.path(), None, mask);
            let g = spec.geometry().unwrap();
            let overlay = render::overlay(&spec, &key(true)).unwrap();
            let hole = hole_px((352, 744), g, Quarter::None);
            // Every pixel of the hole is the picture's, but under the
            // foreground's tips (and a pixel round them, its soft edge).
            let mut covered = Vec::new();
            for y in hole.y0..hole.y1 {
                for x in hole.x0..hole.x1 {
                    let (sx, sy) = ((x - hole.x0) as u32, (y - hole.y0) as u32);
                    let (dx, dy) = (sx.min(319 - sx), sy.min(713 - sy));
                    let near_tip = mask.is_some() && dx + dy < 5;
                    let px = overlay.get_pixel(x as u32, y as u32).0;
                    if !near_tip && px[3] != 0 {
                        covered.push((sx, sy, px));
                    }
                }
            }
            assert!(covered.is_empty(), "{name}: {} covered, e.g. {:?}", covered.len(), &covered[..covered.len().min(5)]);
            // A tip is the foreground's, blue.
            if mask.is_some() {
                assert_eq!(overlay.get_pixel(hole.x0 as u32, hole.y0 as u32).0, [255, 0, 0, 255], "{name}");
            }
            // Frame off: the default radius (25.6 here) rounds the screen.
            let bare = render::overlay(&spec, &key(false)).unwrap();
            assert_eq!(bare.get_pixel(3, 3).0, [30, 200, 10, 255], "{name}");
            assert_eq!(bare.get_pixel(160, 357).0[3], 0, "{name}");
        }
    }

    /// Frame off (or no skin), an emulator's screen is rounded by its
    /// corner radius alone.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_emulator_without_a_frame_gets_rounded_corners() {
        let spec = ChromeSpec {
            screen_pt: Some((1080.0, 2400.0)),
            screen_corner_pt: Some(86.4),
            mask: Some(ScreenMask::Rounded),
            frame: None,
        };
        let fill = [10, 200, 30, 255];
        let key = OverlayKey { device_type: "a".into(), size_px: (270, 600), turn: Quarter::None, framed: false, fill };
        let overlay = render::overlay(&spec, &key).unwrap();
        let px = |x: u32, y: u32| overlay.get_pixel(x, y).0;
        // 86.4 at a quarter: corners of 21.6. Its corner pixel is the
        // panel's; along the edges and inside, the picture shows.
        for (x, y) in [(0, 0), (269, 0), (0, 599), (269, 599), (3, 3)] {
            assert_eq!(px(x, y), [30, 200, 10, 255], "{x},{y}");
        }
        for (x, y) in [(135, 0), (0, 300), (269, 300), (135, 599), (135, 300), (10, 10)] {
            assert_eq!(px(x, y)[3], 0, "{x},{y}");
        }
        // A framed overlay needs a frame.
        assert!(render::overlay(&spec, &OverlayKey { framed: true, ..key }).is_none());
    }

    #[test]
    fn rounded_corners_cover_only_outside_the_curve() {
        let keep = outside_corners(40, 60, 10.0);
        let at = |x: usize, y: usize| keep[y * 40 + x];
        // Each corner pixel is covered; the middle and the edges' middles
        // aren't.
        for (x, y) in [(0, 0), (39, 0), (0, 59), (39, 59), (1, 1)] {
            assert_eq!(at(x, y), 255, "{x},{y}");
        }
        for (x, y) in [(20, 30), (20, 0), (0, 30), (39, 30), (20, 59), (10, 0), (0, 10), (5, 5)] {
            assert_eq!(at(x, y), 0, "{x},{y}");
        }
        // On the curve, partly: (2, 3)'s centre is 9.92 from the circle's.
        let edge = at(2, 3);
        assert!(edge > 0 && edge < 255, "{edge}");
        // Square: nothing covered. A radius past half the side is half the side.
        assert!(outside_corners(8, 8, 0.0).iter().all(|&k| k == 0));
        assert_eq!(outside_corners(8, 8, 100.0)[0], 255);
        assert_eq!(outside_corners(8, 8, 100.0)[3 * 8 + 3], 0);
        assert!(outside_corners(0, 0, 4.0).is_empty());
    }

    #[test]
    fn the_picture_sits_exactly_in_the_overlays_hole() {
        let g = FrameGeometry {
            screen_pt: (402.0, 874.0),
            sizing: Insets { left: 18.0, top: 18.0, right: 18.0, bottom: 18.0 },
        };
        for scale in [1.0, 1.5, 2.0, 3.0] {
            for (ox, oy) in [(0.0, 0.0), (8.25, 3.4), (11.3, 7.75)] {
                for turn in [Quarter::None, Quarter::Cw90, Quarter::Cw180, Quarter::Cw270] {
                    let container = Rect { x: ox, y: oy, w: 357.3, h: 731.9 };
                    let p = painted(place(container, (0.0, 0.0), Some(g), turn), Some(g), turn, scale);
                    let od = snap(p.overlay, scale);
                    // Drawn at its own size: no stretching.
                    assert_eq!(od.size(), p.overlay_px, "{scale} {turn:?}");
                    let hole = hole_px(p.overlay_px, g, turn);
                    let pd = snap(p.picture, scale);
                    let at = PxRect { x0: od.x0 + hole.x0, y0: od.y0 + hole.y0, x1: od.x0 + hole.x1, y1: od.y0 + hole.y1 };
                    assert_eq!(pd, at, "{scale} {ox},{oy} {turn:?}");
                }
            }
        }
        // Without a frame the picture and its overlay are one rect.
        let screen = Rect { x: 3.3, y: 1.7, w: 201.25, h: 437.6 };
        let p = painted(Placement { frame: None, screen }, None, Quarter::None, 2.0);
        assert_eq!(snap(p.picture, 2.0), snap(p.overlay, 2.0));
        assert_eq!(snap(p.overlay, 2.0).size(), p.overlay_px);
    }
}
