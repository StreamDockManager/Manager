use super::*;
use ::windows::{
    Devices::Geolocation::{GeolocationAccessStatus, Geolocator, PositionAccuracy},
    Foundation::TimeSpan,
};
// Read the current value
pub async fn current(
    app: tauri::AppHandle,
    cancel: Arc<AtomicBool>,
    permit: &crate::editor_lifecycle::EditorWork,
) -> Result<Fix, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let flag = cancel.clone();
    let setup_permit = permit.worker();
    app.run_on_main_thread(move || {
        let _editor_work = setup_permit;
        if flag.load(Ordering::SeqCst) {
            return;
        }
        let _ = tx.send(
            Geolocator::RequestAccessAsync()
                .map_err(|_| "Location permission request unavailable".to_string()),
        );
    })
    .map_err(|e| e.to_string())?;
    crate::editor_lifecycle::spawn_blocking(permit, move || {
        let end=std::time::Instant::now()+Duration::from_secs(60);
        macro_rules! wait { ($operation:expr) => {{
            let op=$operation;
            loop {
                if cancel.load(Ordering::SeqCst) || std::time::Instant::now()>=end {let _=op.Cancel();return Err("Location request cancelled or timed out".into());}
                match op.Status().map_err(|_|"Location service unavailable".to_string())?.0 {
                    1 | 3 => break op.GetResults().map_err(|_|"Location service could not determine a position".to_string())?,
                    2 => return Err("Location request cancelled".into()),
                    _=>std::thread::sleep(Duration::from_millis(100)),
                }
            }
        }}; }
        let access=rx.recv_timeout(Duration::from_secs(5)).map_err(|_|"Location permission request unavailable".to_string())??;
        if wait!(access)!=GeolocationAccessStatus::Allowed {return Err("Location access denied. Enable location access in Windows Privacy & security settings.".into());}
        let locator=Geolocator::new().map_err(|e|e.to_string())?;
        locator.SetDesiredAccuracy(PositionAccuracy::Default).map_err(|e|e.to_string())?;
        let position=wait!(locator.GetGeopositionAsyncWithAgeAndTimeout(TimeSpan{Duration:300_000_000},TimeSpan{Duration:200_000_000}).map_err(|e|e.to_string())?);
        let coordinate=position.Coordinate().map_err(|e|e.to_string())?;
        let point=coordinate.Point().and_then(|p|p.Position()).map_err(|e|e.to_string())?;
        Ok(Fix{latitude:point.Latitude,longitude:point.Longitude,accuracy_m:coordinate.Accuracy().map_err(|e|e.to_string())?,timestamp:coordinate.Timestamp().map_err(|e|e.to_string())?.UniversalTime as f64/10_000_000.0-11_644_473_600.0})
    }).await.map_err(|e|e.to_string())?
}
