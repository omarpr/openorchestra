//! The simulator's screen, converted for gpui (macOS only).
//!
//! The simulator hands out a `'BGRA'` IOSurface, but gpui's `surface()`
//! element draws only `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`
//! pixel buffers, which its Metal renderer turns into textures through a
//! CVMetalTextureCache. So every frame goes through a `VTPixelTransferSession`
//! into an IOSurface-backed, Metal-compatible 420f buffer from a pool. The
//! spike measured this at about 1.2 ms of CPU per full-size frame, with the
//! work on the hardware scaler rather than the GPU (see "Cost of the two
//! drawing routes" in docs/superpowers/specs/2026-11-22-device-tab-ios-design.md).
//!
//! The simulator's surface stays portrait when the device turns, and gpui
//! can't draw a surface turned, so a turned device's frames are turned here:
//! after the transfer, a `VTPixelRotationSession` turns the 420f buffer into
//! one of the swapped size. That session is macOS 13+ and the app starts on
//! 12, so it is looked up at run time; without it frames come as they are.

use std::ffi::{CStr, c_void};
use std::ptr::{self, NonNull};
use std::sync::OnceLock;

use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, Type,
};
use objc2_core_video::{
    kCVImageBufferYCbCrMatrix_ITU_R_601_4, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, CVPixelBuffer,
    CVPixelBufferCreateWithIOSurface, CVPixelBufferPool,
};
use objc2_io_surface::IOSurfaceRef;
use objc2_video_toolbox::{
    kVTPixelTransferPropertyKey_DestinationYCbCrMatrix, VTPixelTransferSession,
    VTSessionSetProperty,
};

use super::Quarter;

/// Converts BGRA IOSurfaces into 420f pixel buffers, one session per
/// converter.
pub(crate) struct PixelConverter {
    session: CFRetained<VTPixelTransferSession>,
    /// Buffers of the last output's size; rebuilt when the size changes (a
    /// turn, or a different device).
    pool: Option<SizedPool>,
    /// Turns frames upright; made on the first turned frame.
    turner: Option<Turner>,
    /// The rotation session couldn't be had (said once).
    cannot_turn: bool,
}

// SAFETY: the converter owns its sessions and pools outright, and `convert`
// takes `&mut self`, so they're only ever used by one thread at a time; a
// VideoToolbox session and a CVPixelBufferPool may be used from any thread,
// one at a time (the simulator's serial callback queue hops threads too).
// Not `Sync`: shared use would let two threads transfer at once.
unsafe impl Send for PixelConverter {}

struct SizedPool {
    width: usize,
    height: usize,
    pool: CFRetained<CVPixelBufferPool>,
}

impl PixelConverter {
    pub(crate) fn new() -> Result<Self, String> {
        let mut out = ptr::null_mut();
        // SAFETY: a writable out-pointer.
        let status = unsafe { VTPixelTransferSession::create(None, NonNull::from(&mut out)) };
        let session = created(status, out, "VTPixelTransferSessionCreate")?;
        // gpui's surface shader turns YCbCr back into RGB with the BT.601
        // full-range matrix, so the conversion has to use the same one.
        // SAFETY: a matrix property set to one of the CFString matrix
        // constants it documents.
        let status = unsafe {
            let matrix: &CFType = kCVImageBufferYCbCrMatrix_ITU_R_601_4;
            VTSessionSetProperty(
                &session,
                kVTPixelTransferPropertyKey_DestinationYCbCrMatrix,
                Some(matrix),
            )
        };
        if status != 0 {
            return Err(format!("setting the transfer's YCbCr matrix failed: {status}"));
        }
        Ok(Self { session, pool: None, turner: None, cannot_turn: false })
    }

    /// `src` as a new `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`
    /// buffer, turned clockwise by `turn` (so of the swapped size for a
    /// quarter turn). Each call takes a fresh buffer from the pool, so a
    /// buffer already handed out is never written again. Without the
    /// rotation session (macOS 12), `src` comes unturned.
    pub(crate) fn convert(
        &mut self,
        src: &IOSurfaceRef,
        turn: Quarter,
    ) -> Result<CFRetained<CVPixelBuffer>, String> {
        let (width, height) = (src.width(), src.height());
        if width == 0 || height == 0 {
            return Err(format!("empty source surface ({width}×{height})"));
        }
        if turn != Quarter::None {
            self.make_turner()?;
        }
        let (Quarter::Cw90 | Quarter::Cw180 | Quarter::Cw270, Some(turner)) =
            (turn, self.turner.as_mut())
        else {
            return self.transfer(src, width, height);
        };
        // Into a portrait buffer of the turner's own, then turned into one
        // from the output pool. The transfer sets the BT.601 matrix; the
        // turn only moves samples.
        let source = wrap(src)?;
        let portrait = turner.portrait(width, height)?;
        // SAFETY: two live buffers of the same size; the source is BGRA and
        // the destination 420f, a conversion VideoToolbox supports.
        let status = unsafe { self.session.transfer_image(&source, &portrait) };
        if status != 0 {
            return Err(format!("VTPixelTransferSessionTransferImage failed: {status}"));
        }
        let (w, h) = match turn {
            Quarter::Cw90 | Quarter::Cw270 => (height, width),
            Quarter::None | Quarter::Cw180 => (width, height),
        };
        let destination = pooled(&mut self.pool, w, h)?;
        turner.turn(&portrait, &destination, turn)?;
        Ok(destination)
    }

    /// `src` as a 420f buffer of the same size, from the output pool.
    fn transfer(
        &mut self,
        src: &IOSurfaceRef,
        width: usize,
        height: usize,
    ) -> Result<CFRetained<CVPixelBuffer>, String> {
        let source = wrap(src)?;
        let destination = pooled(&mut self.pool, width, height)?;
        // SAFETY: two live buffers of the same size; the source is BGRA and
        // the destination 420f, a conversion VideoToolbox supports.
        let status = unsafe { self.session.transfer_image(&source, &destination) };
        if status != 0 {
            return Err(format!("VTPixelTransferSessionTransferImage failed: {status}"));
        }
        Ok(destination)
    }

    /// Makes the turner on first use. Without a rotation session on this
    /// macOS there is none, which is logged once.
    fn make_turner(&mut self) -> Result<(), String> {
        if self.turner.is_some() || self.cannot_turn {
            return Ok(());
        }
        match rotation_api() {
            Some(api) => self.turner = Some(Turner::new(api)?),
            None => {
                self.cannot_turn = true;
                log::info!("device: no VTPixelRotationSession; a turned device is drawn sideways");
            }
        }
        Ok(())
    }
}

impl Drop for PixelConverter {
    fn drop(&mut self) {
        // SAFETY: the session is ours and isn't used again.
        unsafe { self.session.invalidate() };
    }
}

/// A fresh buffer of `width` × `height` from `slot`'s pool, which is
/// rebuilt first if its size differs.
fn pooled(
    slot: &mut Option<SizedPool>,
    width: usize,
    height: usize,
) -> Result<CFRetained<CVPixelBuffer>, String> {
    let pool = match slot.take() {
        Some(pool) if (pool.width, pool.height) == (width, height) => pool,
        _ => SizedPool::new(width, height)?,
    };
    let pool = slot.insert(pool);
    let mut out = ptr::null_mut();
    // SAFETY: a live pool and a writable out-pointer.
    let status = unsafe {
        CVPixelBufferPool::create_pixel_buffer(None, &pool.pool, NonNull::from(&mut out))
    };
    created(status, out, "CVPixelBufferPoolCreatePixelBuffer")
}

type RotationCreate = unsafe extern "C-unwind" fn(*const c_void, *mut *mut CFType) -> i32;
type RotationRotate =
    unsafe extern "C-unwind" fn(*const CFType, *const CVPixelBuffer, *const CVPixelBuffer) -> i32;
type RotationInvalidate = unsafe extern "C-unwind" fn(*const CFType);

/// VideoToolbox's rotation session (macOS 13+), found with `dlsym` so the
/// app still starts on macOS 12.
struct RotationApi {
    create: RotationCreate,
    rotate: RotationRotate,
    invalidate: RotationInvalidate,
    /// `kVTPixelRotationPropertyKey_Rotation`.
    key: &'static CFString,
    /// `kVTRotation_CW90`, `kVTRotation_180` and `kVTRotation_CCW90`.
    cw90: &'static CFString,
    cw180: &'static CFString,
    ccw90: &'static CFString,
}

// SAFETY: plain function pointers, and CFString constants that are
// immutable for the life of the process.
unsafe impl Send for RotationApi {}
unsafe impl Sync for RotationApi {}

/// The rotation session's functions and constants, looked up once. None
/// when any is missing.
fn rotation_api() -> Option<&'static RotationApi> {
    static API: OnceLock<Option<RotationApi>> = OnceLock::new();
    API.get_or_init(|| {
        let symbol = |name: &CStr| {
            // SAFETY: a C string; RTLD_DEFAULT searches the images already
            // loaded, VideoToolbox (linked) among them.
            let found = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            NonNull::new(found)
        };
        let constant = |name: &CStr| -> Option<&'static CFString> {
            // SAFETY: each name is a `CFStringRef` constant VideoToolbox
            // exports; it lives (and never changes) for the process.
            symbol(name).and_then(|p| unsafe { p.cast::<*const CFString>().read().as_ref() })
        };
        // SAFETY: each symbol is the C function of that name, whose
        // signature the type spells out (VTPixelRotationSession.h).
        unsafe {
            Some(RotationApi {
                create: std::mem::transmute::<*mut c_void, RotationCreate>(
                    symbol(c"VTPixelRotationSessionCreate")?.as_ptr(),
                ),
                rotate: std::mem::transmute::<*mut c_void, RotationRotate>(
                    symbol(c"VTPixelRotationSessionRotateImage")?.as_ptr(),
                ),
                invalidate: std::mem::transmute::<*mut c_void, RotationInvalidate>(
                    symbol(c"VTPixelRotationSessionInvalidate")?.as_ptr(),
                ),
                key: constant(c"kVTPixelRotationPropertyKey_Rotation")?,
                cw90: constant(c"kVTRotation_CW90")?,
                cw180: constant(c"kVTRotation_180")?,
                ccw90: constant(c"kVTRotation_CCW90")?,
            })
        }
    })
    .as_ref()
}

/// A rotation session and the portrait buffers it turns from.
struct Turner {
    api: &'static RotationApi,
    session: CFRetained<CFType>,
    /// The turn the session is set to.
    set: Quarter,
    portrait: Option<SizedPool>,
}

impl Turner {
    fn new(api: &'static RotationApi) -> Result<Self, String> {
        let mut out = ptr::null_mut();
        // SAFETY: the default allocator and a writable out-pointer.
        let status = unsafe { (api.create)(ptr::null(), &mut out) };
        let session = created(status, out, "VTPixelRotationSessionCreate")?;
        Ok(Self { api, session, set: Quarter::None, portrait: None })
    }

    fn portrait(
        &mut self,
        width: usize,
        height: usize,
    ) -> Result<CFRetained<CVPixelBuffer>, String> {
        pooled(&mut self.portrait, width, height)
    }

    /// Turns `src` into `dst`, whose size `turn` already swapped.
    fn turn(
        &mut self,
        src: &CVPixelBuffer,
        dst: &CVPixelBuffer,
        turn: Quarter,
    ) -> Result<(), String> {
        if self.set != turn {
            let value = match turn {
                Quarter::Cw90 => self.api.cw90,
                Quarter::Cw180 => self.api.cw180,
                Quarter::Cw270 => self.api.ccw90,
                Quarter::None => return Err("no turn to make".into()),
            };
            // SAFETY: the rotation property set to one of the CFString
            // constants it documents.
            let status = unsafe { VTSessionSetProperty(&self.session, self.api.key, Some(value)) };
            if status != 0 {
                return Err(format!("setting the rotation failed: {status}"));
            }
            self.set = turn;
        }
        // SAFETY: a live session and two live 420f buffers, `dst` of the
        // size the turn needs.
        let status = unsafe { (self.api.rotate)(&*self.session, src, dst) };
        if status != 0 {
            return Err(format!("VTPixelRotationSessionRotateImage failed: {status}"));
        }
        Ok(())
    }
}

impl Drop for Turner {
    fn drop(&mut self) {
        // SAFETY: the session is ours and isn't used again.
        unsafe { (self.api.invalidate)(&*self.session) };
    }
}

impl SizedPool {
    /// A pool of IOSurface-backed, Metal-compatible 420f buffers, which is
    /// what gpui's CVMetalTextureCache needs.
    ///
    /// No pool attributes: no minimum, and above all no maximum buffer count
    /// or allocation threshold. A buffer goes back to the pool only when its
    /// last reference is released, so one the UI still holds is never handed
    /// out again; the pool allocates another instead of blocking or reusing.
    fn new(width: usize, height: usize) -> Result<Self, String> {
        let format = CFNumber::new_i64(i64::from(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange));
        let (w, h) = (CFNumber::new_i64(width as i64), CFNumber::new_i64(height as i64));
        // An empty dictionary asks for IOSurface backing with default
        // properties.
        let io_surface = CFDictionary::<CFString, CFType>::empty();
        // SAFETY: CoreVideo's own attribute-key constants.
        let keys = unsafe {
            [
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferWidthKey,
                kCVPixelBufferHeightKey,
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferMetalCompatibilityKey,
            ]
        };
        let values: [&CFType; 5] = [&format, &w, &h, &io_surface, CFBoolean::new(true)];
        let attributes = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        let mut out = ptr::null_mut();
        // SAFETY: pixel-buffer attributes of the documented value types, and
        // a writable out-pointer.
        let status = unsafe {
            CVPixelBufferPool::create(
                None,
                None,
                Some(attributes.as_opaque()),
                NonNull::from(&mut out),
            )
        };
        let pool = created(status, out, "CVPixelBufferPoolCreate")?;
        Ok(Self { width, height, pool })
    }
}

/// A converted frame on its way from the simulator's callback queue to the
/// UI thread.
pub(crate) struct SendPixelBuffer(pub CFRetained<CVPixelBuffer>);

// SAFETY: CoreFoundation retain and release are thread-safe, and nothing
// writes into the buffer once it's handed out: each `convert` takes a fresh
// buffer from the pool, and the pool never reissues one that is still
// referenced (see `SizedPool::new`).
unsafe impl Send for SendPixelBuffer {}

/// `surface` as a CVPixelBuffer (no copy), for the transfer's source.
fn wrap(surface: &IOSurfaceRef) -> Result<CFRetained<CVPixelBuffer>, String> {
    let mut out = ptr::null_mut();
    // SAFETY: a live surface, no attributes, and a writable out-pointer.
    let status =
        unsafe { CVPixelBufferCreateWithIOSurface(None, surface, None, NonNull::from(&mut out)) };
    created(status, out, "CVPixelBufferCreateWithIOSurface")
}

/// Takes ownership of what a CoreFoundation `Create` call returned.
fn created<T: Type>(status: i32, out: *mut T, call: &str) -> Result<CFRetained<T>, String> {
    if status != 0 {
        return Err(format!("{call} failed: {status}"));
    }
    let out = NonNull::new(out).ok_or_else(|| format!("{call} returned nothing"))?;
    // SAFETY: a successful Create call hands over its +1 reference.
    Ok(unsafe { CFRetained::from_raw(out) })
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CFDictionary, CFNumber, CFString, CFType};
    use objc2_core_video::{
        kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        kCVReturnSuccess, CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane,
        CVPixelBufferGetBytesPerRow, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
        CVPixelBufferGetIOSurface, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
        CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };
    use objc2_io_surface::{
        kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
    };

    use super::*;

    const GREEN: [u8; 4] = [0, 255, 0, 255]; // B, G, R, A
    const RED: [u8; 4] = [0, 0, 255, 255];
    /// Y, Cb, Cr of pure green under BT.601 full range, the matrix gpui's
    /// surface shader inverts. Left to itself VideoToolbox uses BT.709 and
    /// gives 182, 30, 12 (measured, at both sizes tested here).
    const GREEN_601: [u8; 3] = [150, 44, 21];
    const RED_601: [u8; 3] = [76, 85, 255];

    /// A `'BGRA'` IOSurface filled with one colour.
    fn bgra_surface(width: usize, height: usize, bgra: [u8; 4]) -> CFRetained<IOSurfaceRef> {
        painted_surface(width, height, |_, _| bgra)
    }

    /// A `'BGRA'` IOSurface with each pixel's colour from `paint(x, y)`.
    fn painted_surface(
        width: usize,
        height: usize,
        paint: impl Fn(usize, usize) -> [u8; 4],
    ) -> CFRetained<IOSurfaceRef> {
        let number = |n: i64| CFNumber::new_i64(n);
        let (w, h, bpe) = (number(width as i64), number(height as i64), number(4));
        let format = number(i64::from(kCVPixelFormatType_32BGRA));
        // SAFETY: the keys are IOSurface's own property-key constants.
        let keys = unsafe {
            [kIOSurfaceWidth, kIOSurfaceHeight, kIOSurfaceBytesPerElement, kIOSurfacePixelFormat]
        };
        let values: [&CFType; 4] = [&w, &h, &bpe, &format];
        let props = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        // SAFETY: the properties dictionary holds the documented key types.
        let surface = unsafe { IOSurfaceRef::new(props.as_opaque()) }.expect("IOSurfaceCreate");
        let buffer = wrap(&surface).expect("CVPixelBufferCreateWithIOSurface");
        locked(&buffer, CVPixelBufferLockFlags::empty(), |buffer| {
            let base = CVPixelBufferGetBaseAddress(buffer).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRow(buffer);
            for y in 0..height {
                for x in 0..width {
                    let bgra = paint(x, y);
                    // SAFETY: the buffer is locked, and (x, y) is a pixel
                    // inside its `height` rows of `stride` bytes.
                    unsafe { base.add(y * stride + x * 4).copy_from(bgra.as_ptr(), 4) };
                }
            }
        });
        surface
    }

    /// Runs `f` with `buffer`'s base address locked.
    fn locked<T>(
        buffer: &CVPixelBuffer,
        flags: CVPixelBufferLockFlags,
        f: impl FnOnce(&CVPixelBuffer) -> T,
    ) -> T {
        // SAFETY: a live buffer; every lock is paired with an unlock with
        // the same flags.
        assert_eq!(unsafe { CVPixelBufferLockBaseAddress(buffer, flags) }, kCVReturnSuccess);
        let out = f(buffer);
        assert_eq!(unsafe { CVPixelBufferUnlockBaseAddress(buffer, flags) }, kCVReturnSuccess);
        out
    }

    /// Y, Cb, Cr at the middle of a 420f buffer. Checks too that the buffer
    /// is IOSurface-backed, as gpui's Metal texture cache needs.
    fn ycbcr_at_centre(buffer: &CVPixelBuffer) -> [u8; 3] {
        let x = CVPixelBufferGetWidth(buffer) / 2;
        let y = CVPixelBufferGetHeight(buffer) / 2;
        ycbcr_at(buffer, x, y)
    }

    /// Y, Cb, Cr at (x, y) of a 420f buffer, which must be IOSurface-backed.
    fn ycbcr_at(buffer: &CVPixelBuffer, x: usize, y: usize) -> [u8; 3] {
        assert!(CVPixelBufferGetIOSurface(Some(buffer)).is_some(), "not IOSurface-backed");
        locked(buffer, CVPixelBufferLockFlags::ReadOnly, |buffer| {
            let luma = CVPixelBufferGetBaseAddressOfPlane(buffer, 0).cast::<u8>();
            let chroma = CVPixelBufferGetBaseAddressOfPlane(buffer, 1).cast::<u8>();
            let luma_stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
            let chroma_stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1);
            // SAFETY: the buffer is locked; (x, y) is inside plane 0 (a byte
            // per pixel) and (x / 2, y / 2) inside plane 1 (Cb, Cr per 2×2).
            unsafe {
                let pair = chroma.add((y / 2) * chroma_stride + (x / 2) * 2);
                [*luma.add(y * luma_stride + x), *pair, *pair.add(1)]
            }
        })
    }

    fn assert_close(got: [u8; 3], want: [u8; 3]) {
        let close = got.iter().zip(want).all(|(&g, w)| g.abs_diff(w) <= 3);
        assert!(close, "Y/Cb/Cr {got:?}, want about {want:?}");
    }

    fn assert_420f(buffer: &CVPixelBuffer, width: usize, height: usize) {
        assert_eq!(
            CVPixelBufferGetPixelFormatType(buffer),
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
        );
        let size = (CVPixelBufferGetWidth(buffer), CVPixelBufferGetHeight(buffer));
        assert_eq!(size, (width, height));
    }

    #[test]
    fn converts_bgra_to_full_range_420() {
        let mut converter = PixelConverter::new().unwrap();
        let out = converter.convert(&bgra_surface(64, 128, GREEN), Quarter::None).unwrap();
        assert_420f(&out, 64, 128);
        assert_close(ycbcr_at_centre(&out), GREEN_601);
    }

    #[test]
    fn a_resized_source_gets_a_resized_output() {
        let mut converter = PixelConverter::new().unwrap();
        let first = converter.convert(&bgra_surface(64, 128, GREEN), Quarter::None).unwrap();
        assert_420f(&first, 64, 128);
        let second = converter.convert(&bgra_surface(128, 64, GREEN), Quarter::None).unwrap();
        assert_420f(&second, 128, 64);
    }

    /// The colours at the simulator's real size too (an iPhone 18 Pro's
    /// screen), in case VideoToolbox chose a matrix by image size.
    #[test]
    fn full_size_frames_use_bt601() {
        let mut converter = PixelConverter::new().unwrap();
        let out = converter.convert(&bgra_surface(1206, 2622, GREEN), Quarter::None).unwrap();
        assert_420f(&out, 1206, 2622);
        assert_close(ycbcr_at_centre(&out), GREEN_601);
    }

    /// What `SendPixelBuffer`'s safety rests on: a buffer the UI still holds
    /// is never written again.
    #[test]
    fn a_held_buffer_is_not_reused() {
        let mut converter = PixelConverter::new().unwrap();
        let held = converter.convert(&bgra_surface(64, 128, GREEN), Quarter::None).unwrap();
        let next = converter.convert(&bgra_surface(64, 128, RED), Quarter::None).unwrap();
        assert_ne!(NonNull::from(&*held), NonNull::from(&*next));
        assert_close(ycbcr_at_centre(&held), GREEN_601);
        assert_close(ycbcr_at_centre(&next), RED_601);
    }

    /// A portrait 64×128 source, green but for its top-left 32×32 corner,
    /// which is red: every turn puts that corner somewhere else.
    fn marked_corner() -> CFRetained<IOSurfaceRef> {
        painted_surface(64, 128, |x, y| if x < 32 && y < 32 { RED } else { GREEN })
    }

    /// Where the red corner lands, and the output's size, for each turn.
    #[test]
    fn a_turned_frame_is_drawn_upright() {
        let mut converter = PixelConverter::new().unwrap();
        // (turn, output width × height, a point in the red corner, one in
        // the green): clockwise takes the top-left to the top-right.
        let cases = [
            (Quarter::None, (64, 128), (16, 16), (48, 112)),
            (Quarter::Cw90, (128, 64), (112, 16), (16, 48)),
            (Quarter::Cw180, (64, 128), (48, 112), (16, 16)),
            (Quarter::Cw270, (128, 64), (16, 48), (112, 16)),
        ];
        for (turn, (w, h), red, green) in cases {
            let out = converter.convert(&marked_corner(), turn).unwrap();
            assert_420f(&out, w, h);
            assert_close(ycbcr_at(&out, red.0, red.1), RED_601);
            assert_close(ycbcr_at(&out, green.0, green.1), GREEN_601);
        }
    }

    /// Turning keeps gpui's BT.601 colours, at the simulator's real size too.
    #[test]
    fn a_full_size_turned_frame_keeps_bt601() {
        let mut converter = PixelConverter::new().unwrap();
        let out = converter.convert(&bgra_surface(1206, 2622, GREEN), Quarter::Cw90).unwrap();
        assert_420f(&out, 2622, 1206);
        assert_close(ycbcr_at_centre(&out), GREEN_601);
        // And back to portrait on the same converter.
        let out = converter.convert(&bgra_surface(1206, 2622, RED), Quarter::None).unwrap();
        assert_420f(&out, 1206, 2622);
        assert_close(ycbcr_at_centre(&out), RED_601);
    }
}
