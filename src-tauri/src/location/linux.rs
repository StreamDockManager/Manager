use super::*;
use ashpd::desktop::location::{Accuracy, CreateSessionOptions, LocationProxy};
use futures_util::StreamExt;
// Read the current value
pub async fn current(cancel: Arc<AtomicBool>) -> Result<Fix, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("Location request cancelled".into());
    }
    let cancelled = async {
        loop {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    tokio::pin!(cancelled);
    let setup = async {
        let proxy = LocationProxy::new().await.map_err(|_| "Location portal unavailable. Enable your desktop location service or enter coordinates.".to_string())?;
        let session = proxy
            .create_session(CreateSessionOptions::default().set_accuracy(Accuracy::Neighborhood))
            .await
            .map_err(|_| "Location session could not be created".to_string())?;
        Ok::<_, String>((proxy, session))
    };
    let (proxy, session) = tokio::select! {
        result = setup => result?,
        _ = &mut cancelled => return Err("Location request cancelled".into()),
        _ = tokio::time::sleep(Duration::from_secs(10)) => return Err("Location portal did not respond. Try again or enter coordinates.".into()),
    };
    if cancel.load(Ordering::SeqCst) {
        let _ = session.close().await;
        return Err("Location request cancelled".into());
    }
    let operation = async {
        let mut updates = proxy
            .receive_location_updated()
            .await
            .map_err(|e| e.to_string())?;
        let request = proxy
            .start(&session, None, Default::default())
            .await
            .map_err(|_| "Location permission denied or service unavailable".to_string())?;
        request
            .response()
            .map_err(|_| "Location permission denied or service unavailable".to_string())?;
        while let Some(location) = updates.next().await {
            let timestamp = location.timestamp();
            let fix = Fix {
                latitude: location.latitude(),
                longitude: location.longitude(),
                accuracy_m: location.accuracy(),
                timestamp: timestamp.as_secs_f64(),
            };
            if validate_fix(fix.clone()).is_ok() {
                return Ok(fix);
            }
        }
        Err("Location service stopped before obtaining a fix".into())
    };
    let result = tokio::select! {result=operation=>result,_= &mut cancelled=>Err("Location request cancelled".into()),_=tokio::time::sleep(Duration::from_secs(60))=>Err("Location request timed out. Try again or enter coordinates.".into())};
    let _ = session.close().await;
    result
}
