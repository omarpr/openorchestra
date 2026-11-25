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

use super::simctl::DeviceList;
use super::{AndroidRunning, DeviceInfo, Platform};

/// What the picker shows for an Android device.
const OS: &str = "Android";

/// `list` with the Android group: the `running` emulators and the AVDs in
/// `avds` that aren't running, each sorted by name.
pub(crate) fn with_android(
    mut list: DeviceList,
    running: Vec<AndroidRunning>,
    avds: Vec<String>,
) -> DeviceList {
    let device = |avd_id: &str, name: String, android: Option<AndroidRunning>| DeviceInfo {
        udid: super::android_device_id(avd_id),
        name,
        os: OS.to_string(),
        device_type: String::new(),
        platform: Platform::Android,
        android,
    };
    let mut stopped: Vec<DeviceInfo> = avds
        .iter()
        .filter(|avd| !running.iter().any(|r| &r.avd_id == *avd))
        .map(|avd| device(avd, avd.replace('_', " "), None))
        .collect();
    stopped.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.udid.cmp(&b.udid)));
    let mut running: Vec<DeviceInfo> = running
        .into_iter()
        .map(|r| device(&r.avd_id.clone(), r.name.clone(), Some(r)))
        .collect();
    running.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.udid.cmp(&b.udid)));
    list.android_running = running;
    list.android_stopped = stopped;
    list
}

/// The Android emulators running now and the stopped AVDs (none without an
/// SDK), in a list of their own. Blocks briefly (a directory read and
/// `emulator -list-avds`, about 25 ms): call it off the main thread, and
/// only when the simulators are listed (the tab is on screen).
pub(crate) fn list_android() -> DeviceList {
    let list = DeviceList::default();
    let avds = sdk::list_avds().unwrap_or_else(|e| {
        log::debug!("device: listing AVDs failed: {e}");
        Vec::new()
    });
    with_android(list, discovery::running_emulators(), avds)
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
    fn running_emulators_and_stopped_avds_join_the_list() {
        let ios = DeviceInfo { udid: "U".into(), name: "iPhone 18".into(), ..Default::default() };
        let list = DeviceList { booted: vec![ios.clone()], ..Default::default() };
        let pixel = running("Pixel_9_Pro_API_35", "Pixel 9 Pro API 35");
        let got = with_android(
            list,
            vec![pixel.clone()],
            vec!["Pixel_9_Pro_API_35".into(), "Medium_Phone_API_35".into(), "A_Tablet".into()],
        );
        // The simulators are untouched.
        assert_eq!(got.booted, vec![ios.clone()]);
        assert_eq!(
            got.android_running,
            vec![DeviceInfo {
                udid: "android:Pixel_9_Pro_API_35".into(),
                name: "Pixel 9 Pro API 35".into(),
                os: "Android".into(),
                device_type: String::new(),
                platform: Platform::Android,
                android: Some(pixel),
            }]
        );
        // A running AVD isn't also stopped; the rest are, named from their
        // ids, by name.
        let stopped: Vec<(&str, &str)> =
            got.android_stopped.iter().map(|d| (d.udid.as_str(), d.name.as_str())).collect();
        assert_eq!(
            stopped,
            [("android:A_Tablet", "A Tablet"), ("android:Medium_Phone_API_35", "Medium Phone API 35")]
        );
        assert!(got.android_stopped.iter().all(|d| d.platform == Platform::Android && d.android.is_none()));
        // Running: simulators first.
        let ids: Vec<String> = got.running().into_iter().map(|d| d.udid).collect();
        assert_eq!(ids, ["U", "android:Pixel_9_Pro_API_35"]);
    }
}
