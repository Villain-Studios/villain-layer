//! Settings → Phone (PHONE-*): the ways in, typing, pairing and forgetting.

use tauri::{AppHandle, Emitter};

use crate::error::Result;
use crate::phone::{self, PairingView, PhoneStatus};

/// Off the command thread: it lists the network interfaces.
#[tauri::command]
pub async fn phone_status(app: AppHandle) -> Result<PhoneStatus> {
    super::blocking(app, |state| Ok(phone::status(state))).await
}

/// Switch the ways in and typing, and start or stop the server to match.
#[tauri::command]
pub async fn set_phone_access(app: AppHandle, tailscale: bool, home: bool, typing: bool) -> Result<PhoneStatus> {
    super::blocking(app.clone(), move |state| {
        state.config.update(|c| {
            c.phone.tailscale = tailscale;
            c.phone.home = home;
            c.phone.typing = typing;
        })
    })
    .await?;
    phone::apply(app.clone()).await;
    super::blocking(app, |state| Ok(phone::status(state))).await
}

#[tauri::command]
pub fn phone_pair(app: AppHandle) -> Result<PairingView> {
    phone::start_pairing(&app)
}

/// Off the command thread: the token is deleted from the keychain, which
/// can stop and ask.
#[tauri::command]
pub async fn phone_forget(app: AppHandle, id: String) -> Result<()> {
    super::blocking(app.clone(), move |state| phone::forget(state, &id)).await?;
    let _ = app.emit("phone:changed", ());
    Ok(())
}
