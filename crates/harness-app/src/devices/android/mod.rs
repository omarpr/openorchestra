//! Android emulator support for the Device tab: finding running emulators
//! ([`discovery`]) and the AVDs the SDK knows ([`sdk`]), adding them to the
//! simulator list, streaming an emulator's picture ([`stream`]), its input
//! ([`input`]) and bar ([`controls`]), and its device frame ([`skin`]).

pub(crate) mod controls;
pub(crate) mod discovery;
pub(crate) mod grpc;
pub(crate) mod input;
pub(crate) mod sdk;
pub(crate) mod skin;
pub(crate) mod stream;

use super::{AndroidInventory, AndroidRunning, DeviceInfo, Platform};

/// What the picker shows for an Android device.
const OS: &str = "Android";

/// The running emulators and the configured AVDs, each sorted by name.
fn inventory(
    running: Vec<AndroidRunning>,
    avds: Result<Vec<String>, String>,
) -> AndroidInventory {
    let device = |avd_id: &str, name: String, android: Option<AndroidRunning>| DeviceInfo {
        udid: super::android_device_id(avd_id),
        name,
        os: OS.to_string(),
        device_type: String::new(),
        platform: Platform::Android,
        android,
    };
    let configured = avds.map(|avds| {
        let mut avds: Vec<_> = avds
            .iter()
            .map(|avd| device(avd, avd.replace('_', " "), None))
            .collect();
        avds.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.udid.cmp(&b.udid)));
        avds
    });
    let mut running: Vec<DeviceInfo> = running
        .into_iter()
        .map(|r| device(&r.avd_id.clone(), r.name.clone(), Some(r)))
        .collect();
    running.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.udid.cmp(&b.udid)));
    AndroidInventory { running, configured }
}

/// The Android emulators running now and the stopped AVDs (none without an
/// SDK), in a list of their own. Blocks briefly (a directory read and
/// `emulator -list-avds`, about 25 ms): call it off the main thread, and
/// only when the simulators are listed (the tab is on screen).
pub(crate) fn list_android() -> AndroidInventory {
    inventory(discovery::running_emulators(), sdk::list_avds())
}

#[cfg(test)]
mod tests {
    use super::grpc::{image_format::ImgFormat, ImageFormat};
    use super::*;
    use crate::devices::{android_device_id, avd_id_of};
    use prost::Message;

    #[test]
    fn the_generated_client_builds_a_request() {
        let req = ImageFormat {
            format: ImgFormat::Rgba8888 as i32,
            width: 400,
            height: 890,
            ..Default::default()
        };
        let back = ImageFormat::decode(req.encode_to_vec().as_slice()).unwrap();
        assert_eq!(back.format, ImgFormat::Rgba8888 as i32);
        assert_eq!((back.width, back.height), (400, 890));
    }

    fn running(avd_id: &str, name: &str) -> AndroidRunning {
        AndroidRunning {
            pid: 7,
            avd_id: avd_id.into(),
            name: name.into(),
            grpc_port: 8554,
            token: Some("t".into()),
            serial_port: 5554,
            avd_dir: "/a".into(),
        }
    }

    #[test]
    fn android_ids_never_collide_with_a_udid() {
        let id = android_device_id("Pixel_9_Pro_API_35");
        assert_eq!(id, "android:Pixel_9_Pro_API_35");
        assert_eq!(Platform::of_id(&id), Platform::Android);
        assert_eq!(avd_id_of(&id), Some("Pixel_9_Pro_API_35"));
        let udid = "0F291397-02EB-448E-BDFF-320883371EB1";
        assert_eq!(Platform::of_id(udid), Platform::Ios);
        assert_eq!(avd_id_of(udid), None);
    }

    #[test]
    fn running_discovery_survives_an_avd_listing_failure() {
        let pixel = running("Pixel_9_Pro_API_35", "Pixel 9 Pro API 35");
        let got = inventory(vec![pixel.clone()], Err("timeout".into()));
        let dev = &got.running[0];
        assert_eq!(dev.udid, "android:Pixel_9_Pro_API_35");
        assert_eq!(dev.os, "Android");
        assert_eq!(dev.android.as_ref(), Some(&pixel));
        assert_eq!(got.configured, Err("timeout".into()));
    }
}
