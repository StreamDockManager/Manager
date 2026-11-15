//! WASAPI callbacks invalidate only. Registration, default-endpoint rebinding,
//! authoritative reads and COM teardown all belong to the shared worker
use super::{Signal, Watch};
use std::sync::Arc;
use windows::{
    core::{implement, Result, PCWSTR},
    Win32::{
        Foundation::PROPERTYKEY,
        Media::Audio::{
            eMultimedia, eRender, EDataFlow, ERole,
            Endpoints::{
                IAudioEndpointVolume, IAudioEndpointVolumeCallback,
                IAudioEndpointVolumeCallback_Impl,
            },
            IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
            MMDeviceEnumerator, AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE,
        },
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
        },
    },
};

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback(Arc<Signal>);
impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    // Observe the audio endpoint volume notification
    fn OnNotify(&self, _: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        self.0.invalidate(false);
        Ok(())
    }
}
#[implement(IMMNotificationClient)]
struct DeviceCallback(Arc<Signal>);
impl IMMNotificationClient_Impl for DeviceCallback_Impl {
    // Observe an audio endpoint state change
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
        self.0.invalidate(true);
        Ok(())
    }
    // Observe the newly available audio endpoint
    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
        self.0.invalidate(true);
        Ok(())
    }
    // Observe removal of an audio endpoint
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
        self.0.invalidate(true);
        Ok(())
    }
    // Observe a change to the default audio endpoint
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eMultimedia {
            self.0.invalidate(true);
        }
        Ok(())
    }
    // Observe changes to an audio endpoint property
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
        self.0.invalidate(false);
        Ok(())
    }
}
struct Apartment;
impl Drop for Apartment {
    // Release this Windows owner when it leaves scope
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
pub(super) struct Subscription {
    // COM references must be released before this worker leaves its apartment
    devices: IMMDeviceEnumerator,
    endpoint: IAudioEndpointVolume,
    device_callback: IMMNotificationClient,
    volume_callback: IAudioEndpointVolumeCallback,
    volume_registered: bool,
    device_registered: bool,
    _apartment: Apartment,
}
impl Subscription {
    // Initialize Windows state from the supplied inputs
    pub fn new(signal: Arc<Signal>) -> std::result::Result<Self, String> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
            .map_err(|_| "Audio observer COM apartment unavailable")?;
        let apartment = Apartment;
        let result = (|| -> Result<Self> {
            let devices: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
            let endpoint: IAudioEndpointVolume = unsafe {
                devices
                    .GetDefaultAudioEndpoint(eRender, eMultimedia)?
                    .Activate(CLSCTX_ALL, None)?
            };
            let mut value = Self {
                devices,
                endpoint,
                device_callback: DeviceCallback(signal.clone()).into(),
                volume_callback: VolumeCallback(signal).into(),
                volume_registered: false,
                device_registered: false,
                _apartment: apartment,
            };
            unsafe {
                value
                    .devices
                    .RegisterEndpointNotificationCallback(&value.device_callback)?
            };
            value.device_registered = true;
            unsafe {
                value
                    .endpoint
                    .RegisterControlChangeNotify(&value.volume_callback)?
            };
            value.volume_registered = true;
            Ok(value)
        })();
        result.map_err(|_| "Default audio output notifications unavailable".into())
    }
}
impl Watch for Subscription {
    // Report whether the Windows observation is healthy
    fn healthy(&mut self) -> bool {
        true
    }
}
impl Drop for Subscription {
    // Release this Windows owner when it leaves scope
    fn drop(&mut self) {
        unsafe {
            if self.volume_registered {
                let _ = self
                    .endpoint
                    .UnregisterControlChangeNotify(&self.volume_callback);
            }
            if self.device_registered {
                let _ = self
                    .devices
                    .UnregisterEndpointNotificationCallback(&self.device_callback);
            }
        }
    }
}
