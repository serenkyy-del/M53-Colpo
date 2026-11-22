//! Workstation-local keyboard pedal configuration and repeat suppression.
use serde::{Deserialize, Serialize};
pub const MODEL: &str = "FS2016BT1_D";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub key: u8,
    #[serde(default = "sound_default")]
    pub sound: bool,
}
fn sound_default() -> bool {
    true
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            key: 13,
            sound: true,
        }
    }
}
impl Config {
    /// One short, asynchronous tone for an accepted press, never for key repeats.
    pub fn feedback(self) {
        if !self.sound {
            return;
        }
        #[cfg(windows)]
        unsafe {
            #[link(name = "winmm")]
            unsafe extern "system" {
                fn PlaySoundW(sound: *const u8, module: *mut core::ffi::c_void, flags: u32) -> i32;
            }
            // PCM is static for the entire asynchronous playback lifetime.
            let _ = PlaySoundW(
                CLICK.as_ptr(),
                core::ptr::null_mut(),
                0x0001 | 0x0004 | 0x0002,
            );
        }
    }
    pub fn validate(self) -> Result<Self, String> {
        if (13..=22).contains(&self.key) {
            Ok(self)
        } else {
            Err("Выберите клавишу от F13 до F22".into())
        }
    }
    pub fn matches(self, key: &str) -> bool {
        key == format!("f{}", self.key)
    }
}
// Generated PCM rather than a device/system beep: no disk access or extra runtime.
#[cfg(windows)]
static CLICK: [u8; 44 + 2205 * 2] = click_wave();
#[cfg(windows)]
const fn click_wave() -> [u8; 44 + 2205 * 2] {
    let mut wave = [0u8; 44 + 2205 * 2];
    let header = *b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x44\xac\0\0\x88\x58\x01\0\x02\0\x10\0data\0\0\0\0";
    let mut i = 0;
    while i < 44 {
        wave[i] = header[i];
        i += 1;
    }
    let riff = ((wave.len() - 8) as u32).to_le_bytes();
    let size = ((wave.len() - 44) as u32).to_le_bytes();
    i = 0;
    while i < 4 {
        wave[4 + i] = riff[i];
        wave[40 + i] = size[i];
        i += 1;
    }
    i = 0;
    while i < 2205 {
        // Quiet triangular 882 Hz tone with a fade in/out (50 ms).
        let phase = (i % 50) as i32;
        let triangle = if phase < 25 {
            phase * 2 - 25
        } else {
            75 - phase * 2
        };
        let fade = if i < 200 {
            i
        } else if i > 2005 {
            2205 - i
        } else {
            200
        };
        let sample = (triangle * 160 * fade as i32 / 200) as i16;
        let bytes = sample.to_le_bytes();
        wave[44 + i * 2] = bytes[0];
        wave[45 + i * 2] = bytes[1];
        i += 1;
    }
    wave
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    Checking,
    Connected,
    Disconnected,
    NotPaired,
    Unknown(String),
}
impl Connection {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Checking => "Проверка…",
            Self::Connected => "Подключена",
            Self::Disconnected => "Нет связи",
            Self::NotPaired => "Не сопряжена",
            Self::Unknown(_) => "Статус неизвестен",
        }
    }
}
#[derive(Default)]
pub struct Latch {
    down: bool,
}
impl Latch {
    pub fn reset(&mut self) {
        self.down = false;
    }
    pub fn release(&mut self, config: Config, key: &str) {
        if config.matches(key) {
            self.reset();
        }
    }
    pub fn press(&mut self, config: Config, key: &str, held: bool, allowed: bool) -> bool {
        if !config.matches(key) {
            return false;
        }
        let repeated = self.down || held;
        self.down = true;
        config.enabled && allowed && !repeated
    }
}

#[cfg(windows)]
pub fn probe() -> Connection {
    use windows::Devices::{
        Bluetooth::{BluetoothDevice, BluetoothLEDevice},
        Enumeration::{DeviceInformation, DeviceInformationKind},
    };
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
    if let Err(error) = unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
        return Connection::Unknown(format!("WinRT: {error}"));
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                RoUninitialize();
            }
        }
    }
    let _apartment = Apartment;
    let find = |selector: windows::core::Result<windows::core::HSTRING>,
                le: bool|
     -> Result<(bool, bool), String> {
        let selector = selector.map_err(|e| e.to_string())?;
        let devices = DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(
            &selector,
            None,
            DeviceInformationKind::AssociationEndpoint,
        )
        .and_then(|op| op.join())
        .map_err(|e| e.to_string())?;
        let mut paired = false;
        for i in 0..devices.Size().map_err(|e| e.to_string())? {
            let device = devices.GetAt(i).map_err(|e| e.to_string())?;
            if device
                .Name()
                .map_err(|e| e.to_string())?
                .to_string()
                .eq_ignore_ascii_case(MODEL)
            {
                paired = true;
                // Windows may retain IsConnected on an unavailable BLE endpoint.
                // FromId only obtains the device object; no GATT service/session
                // or MaintainConnection is requested by this read-only probe.
                let id = device.Id().map_err(|e| e.to_string())?;
                let status = if le {
                    BluetoothLEDevice::FromIdAsync(&id)
                        .and_then(|op| op.join())
                        .and_then(|live| {
                            let status = live.ConnectionStatus();
                            let _ = live.Close();
                            status
                        })
                } else {
                    BluetoothDevice::FromIdAsync(&id)
                        .and_then(|op| op.join())
                        .and_then(|live| {
                            let status = live.ConnectionStatus();
                            let _ = live.Close();
                            status
                        })
                };
                if live_connected(status)? {
                    return Ok((true, true));
                }
            }
        }
        Ok((paired, false))
    };
    let devices = [
        find(
            BluetoothDevice::GetDeviceSelectorFromPairingState(true),
            false,
        ),
        find(
            BluetoothLEDevice::GetDeviceSelectorFromPairingState(true),
            true,
        ),
    ];
    if devices.iter().any(|r| matches!(r, Ok((_, true)))) {
        return Connection::Connected;
    }
    if let Some(error) = devices.iter().find_map(|r| r.as_ref().err()) {
        return Connection::Unknown(error.clone());
    }
    if devices.iter().any(|r| matches!(r, Ok((true, false)))) {
        Connection::Disconnected
    } else {
        Connection::NotPaired
    }
}
#[cfg(windows)]
fn live_connected(
    status: windows::core::Result<windows::Devices::Bluetooth::BluetoothConnectionStatus>,
) -> Result<bool, String> {
    match status {
        Ok(value) => Ok(value == windows::Devices::Bluetooth::BluetoothConnectionStatus::Connected),
        // A successful null FromId result is unavailable, never "connected".
        Err(error) if error.code().0 == 0 => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}
#[cfg(not(windows))]
pub fn probe() -> Connection {
    Connection::Unknown("Проверка Bluetooth доступна в Windows".into())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColposcopeConfig {
    pub selected: Option<String>,
    pub custom_models: Vec<String>,
}
pub const COLPOSCOPES: [&str; 2] = ["Sensitec VC-200", "Sensitec SLC-2000"];
impl ColposcopeConfig {
    pub fn validate(self) -> Result<Self, String> {
        if self.custom_models.len() > 100
            || self
                .custom_models
                .iter()
                .chain(self.selected.iter())
                .any(|s| s.trim().is_empty() || s.len() > 200 || s.chars().any(char::is_control))
        {
            return Err("Некорректное название кольпоскопа".into());
        }
        Ok(self)
    }
}

pub enum Request {
    SaveVideo(Option<crate::video::Config>),
    SaveColposcope(ColposcopeConfig),
    Save(Config),
    Refresh,
}
pub enum Event {
    Video(Result<Option<crate::video::Config>, String>),
    Colposcope(Result<ColposcopeConfig, String>),
    Loaded(Result<Config, String>),
    Saved(Result<Config, String>),
    Status(Connection),
}
/// Owns disk/Bluetooth operations off the GPUI and clinical worker threads.
pub fn start() -> (
    std::sync::mpsc::Sender<Request>,
    std::sync::mpsc::Receiver<Event>,
) {
    use std::{
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };
    let (tx, rx) = mpsc::channel();
    let (events, out) = mpsc::channel();
    std::thread::spawn(move || {
        #[cfg(windows)]
        let store = std::env::var_os("LOCALAPPDATA")
            .ok_or("LOCALAPPDATA недоступен".to_owned())
            .and_then(|p| {
                crate::cache::Store::new(
                    std::path::PathBuf::from(p)
                        .join(crate::APP_DATA_DIR)
                        .join("hardware"),
                    Arc::new(crate::cache::Dpapi),
                )
            });
        #[cfg(not(windows))]
        let store: Result<crate::cache::Store, String> =
            Err("Профиль оборудования сохраняется в Windows".into());
        let loaded = store
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|s| match s.read("pedal")? {
                None => Ok(Config::default()),
                Some(bytes) => serde_json::from_slice::<Config>(&bytes)
                    .map_err(|_| "Повреждены настройки педали".into())
                    .and_then(Config::validate),
            });
        let _ = events.send(Event::Loaded(loaded));
        let video = store
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|s| match s.read("video")? {
                None => Ok(None),
                Some(bytes) => serde_json::from_slice::<Option<crate::video::Config>>(&bytes)
                    .map_err(|e| e.to_string())
                    .and_then(|c| c.map(crate::video::Config::validate).transpose()),
            });
        let _ = events.send(Event::Video(video));
        let equipment =
            store
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|s| match s.read("colposcope")? {
                    None => Ok(ColposcopeConfig::default()),
                    Some(b) => serde_json::from_slice::<ColposcopeConfig>(&b)
                        .map_err(|e| e.to_string())
                        .and_then(ColposcopeConfig::validate),
                });
        let _ = events.send(Event::Colposcope(equipment));
        let interval = Duration::from_secs(5);
        let mut finished = Instant::now() - interval;
        let mut next = Instant::now();
        loop {
            // Sleep until a command or the next actual check, not four times/sec.
            match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(Request::SaveVideo(config)) => {
                    let result = config
                        .map(crate::video::Config::validate)
                        .transpose()
                        .and_then(|c| {
                            let s = store.as_ref().map_err(Clone::clone)?;
                            s.write("video", &serde_json::to_vec(&c).map_err(|e| e.to_string())?)?;
                            Ok(c)
                        });
                    let _ = events.send(Event::Video(result));
                }
                Ok(Request::SaveColposcope(config)) => {
                    let result = config.validate().and_then(|c| {
                        let s = store.as_ref().map_err(Clone::clone)?;
                        s.write(
                            "colposcope",
                            &serde_json::to_vec(&c).map_err(|e| e.to_string())?,
                        )?;
                        Ok(c)
                    });
                    let _ = events.send(Event::Colposcope(result));
                }
                Ok(Request::Save(config)) => {
                    let result = config.validate().and_then(|c| {
                        let s = store.as_ref().map_err(Clone::clone)?;
                        let bytes = serde_json::to_vec(&c).map_err(|_| "Ошибка настроек педали")?;
                        s.write("pedal", &bytes)?;
                        Ok(c)
                    });
                    let _ = events.send(Event::Saved(result));
                }
                // Repeated clicks cannot start parallel probes or a tight loop.
                Ok(Request::Refresh) => {
                    next = Instant::now().max(finished + Duration::from_secs(1));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if Instant::now() >= next {
                if events.send(Event::Status(probe())).is_err() {
                    break;
                }
                finished = Instant::now();
                next = finished + interval;
            }
        }
    });
    (tx, out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn only_live_device_status_can_be_connected() {
        use windows::{
            Devices::Bluetooth::BluetoothConnectionStatus as Status,
            core::{Error, HRESULT},
        };
        assert_eq!(live_connected(Ok(Status::Connected)), Ok(true));
        assert_eq!(live_connected(Ok(Status::Disconnected)), Ok(false));
        assert_eq!(
            live_connected(Err(Error::from_hresult(HRESULT(0)))),
            Ok(false)
        );
        assert!(live_connected(Err(Error::from_hresult(HRESULT(0x80070005u32 as i32)))).is_err());
    }
    #[test]
    fn old_settings_enable_feedback_and_new_settings_preserve_mute() {
        let old: Config = serde_json::from_str(r#"{"enabled":true,"key":13}"#).unwrap();
        assert!(old.sound);
        let muted = Config {
            sound: false,
            ..old
        };
        assert_eq!(
            serde_json::from_slice::<Config>(&serde_json::to_vec(&muted).unwrap()).unwrap(),
            muted
        );
    }
    #[test]
    fn stuck_pedal_ten_million_events_create_one_action() {
        let config = Config {
            enabled: true,
            key: 13,
            sound: true,
        };
        let mut latch = Latch::default();
        let mut actions = usize::from(latch.press(config, "f13", false, true));
        // Also defend against a driver that incorrectly omits the held flag.
        for i in 0..10_000_000 {
            actions += usize::from(latch.press(config, "f13", i % 2 == 0, true));
        }
        assert_eq!(actions, 1);
        latch.release(config, "f14");
        assert!(!latch.press(config, "f13", false, true));
        latch.release(config, "f13");
        assert!(latch.press(config, "f13", false, true));
    }
    #[test]
    fn all_keys_and_repeat_suppression() {
        for key in 13..=22 {
            let config = Config {
                enabled: true,
                key,
                ..Default::default()
            };
            let mut latch = Latch::default();
            let name = format!("f{key}");
            assert!(config.validate().is_ok());
            assert!(latch.press(config, &name, false, true));
            for _ in 0..47 {
                assert!(!latch.press(config, &name, true, true));
            }
            assert!(!latch.press(config, &name, false, true));
            latch.release(config, &name);
            assert!(latch.press(config, &name, false, true));
        }
        assert!(
            Config {
                enabled: true,
                key: 12,
                sound: true
            }
            .validate()
            .is_err()
        );
        assert!(
            Config {
                enabled: true,
                key: 23,
                sound: true
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn blocked_context_cannot_fire_on_repeat_or_other_keys() {
        let c = Config {
            enabled: true,
            key: 13,
            sound: true,
        };
        let mut l = Latch::default();
        assert!(!l.press(c, "f14", false, true));
        assert!(!l.press(c, "f13", false, false));
        assert!(!l.press(c, "f13", true, true));
        l.release(c, "f13");
        assert!(l.press(c, "f13", false, true));
        l.reset();
        assert!(!l.press(c, "f13", true, true));
        l.reset();
        assert!(l.press(c, "f13", false, true));
        l.reset();
        assert!(!l.press(
            Config {
                enabled: false,
                key: 13,
                sound: true
            },
            "f13",
            false,
            true
        ));
    }
}
