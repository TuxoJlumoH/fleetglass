//! Android glue for the Kotlin `FleetglassPlugin` (task 03-02b): Keystore-backed credential
//! sealing, the document picker used by kubeconfig import, and the share sheet (03-04a).
//!
//! Threading: `SecretVault` is synchronous, so [`KeystoreVault`] uses the blocking
//! `PluginHandle::run_mobile_plugin`. That call dispatches to the Android main (UI) thread and
//! waits for the answer, so it must never run on the main thread (it would deadlock). Call the
//! vault from a Tokio worker / `spawn_blocking` (async Tauri commands are fine; sync commands
//! run on the main thread). [`AndroidBridge::pick_file`] is async and safe from async commands.
#![cfg(target_os = "android")]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use fleetglass_core::import::SecretVault;
use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{Manager, Runtime};

const PLUGIN_IDENTIFIER: &str = "dev.fleetglass.Fleetglass";
const PLUGIN_CLASS: &str = "FleetglassPlugin";

/// Same limit as the Kotlin side; checked again here so the bound holds whatever Kotlin returns.
const MAX_PICKED_FILE_BYTES: usize = 1024 * 1024;

/// Registers the Kotlin plugin and manages [`AndroidBridge`] (get a vault with
/// [`AndroidBridge::vault`]).
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("fleetglass-android")
        .setup(|app, api| {
            let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, PLUGIN_CLASS)?;
            app.manage(AndroidBridge(handle));
            Ok(())
        })
        .build()
}

/// Handle to the Kotlin plugin, managed as Tauri state by [`init`].
pub struct AndroidBridge<R: Runtime>(PluginHandle<R>);

#[derive(Serialize)]
struct DataPayload<'a> {
    data: &'a str,
}

#[derive(Deserialize)]
struct DataResponse {
    data: String,
}

#[derive(Deserialize)]
struct PickResponse {
    #[serde(default)]
    cancelled: bool,
    name: Option<String>,
    data: Option<String>,
}

#[derive(Serialize)]
struct Empty {}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SharePayload<'a> {
    title: &'a str,
    file_name: &'a str,
    text: &'a str,
}

impl<R: Runtime> AndroidBridge<R> {
    /// Opens the system document picker. `Ok(None)` = the user cancelled; otherwise the display
    /// name (if the provider gives one) and the bytes (at most 1 MiB; larger files are an error).
    pub async fn pick_file(&self) -> Result<Option<(Option<String>, Vec<u8>)>, String> {
        let resp: PickResponse = self
            .0
            .run_mobile_plugin_async("pickFile", Empty {})
            .await
            .map_err(|e| e.to_string())?;
        if resp.cancelled {
            return Ok(None);
        }
        let b64 = resp
            .data
            .ok_or_else(|| "document picker returned no data".to_string())?;
        // Base64 is 4/3 of the payload: reject before decoding anything oversized.
        if b64.len() > MAX_PICKED_FILE_BYTES.div_ceil(3) * 4 {
            return Err("the selected file is larger than 1 MiB".into());
        }
        let bytes = STANDARD
            .decode(b64)
            .map_err(|_| "document picker returned invalid base64".to_string())?;
        if bytes.len() > MAX_PICKED_FILE_BYTES {
            return Err("the selected file is larger than 1 MiB".into());
        }
        Ok(Some((resp.name, bytes)))
    }

    /// Writes `text` to `cacheDir/shared/<file_name>` (Kotlin sanitises the name and rejects
    /// more than 8 MiB) and opens the Android share sheet for it. Async: safe from async
    /// commands (the blocking variant would wait on the main thread).
    pub async fn share_text(&self, title: &str, file_name: &str, text: &str) -> Result<(), String> {
        let _: serde_json::Value = self
            .0
            .run_mobile_plugin_async(
                "shareText",
                SharePayload {
                    title,
                    file_name,
                    text,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// A [`SecretVault`] backed by the same Kotlin plugin.
    pub fn vault(&self) -> KeystoreVault<R> {
        KeystoreVault(self.0.clone())
    }
}

/// AES-256-GCM with a non-exportable Android Keystore key (`fleetglass-credentials-v1`).
///
/// Blocking: never call from the Android main thread (see the module docs).
pub struct KeystoreVault<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> KeystoreVault<R> {
    fn call(&self, command: &str, input: &[u8]) -> fleetglass_core::Result<Vec<u8>> {
        let encoded = STANDARD.encode(input);
        let resp: DataResponse = self
            .0
            .run_mobile_plugin(command, DataPayload { data: &encoded })
            .map_err(|e| fleetglass_core::Error::Vault(format!("keystore {command}: {e}")))?;
        STANDARD.decode(resp.data).map_err(|_| {
            fleetglass_core::Error::Vault(format!("keystore {command}: invalid base64 response"))
        })
    }
}

impl<R: Runtime> SecretVault for KeystoreVault<R> {
    fn seal(&self, plain: &[u8]) -> fleetglass_core::Result<Vec<u8>> {
        self.call("seal", plain)
    }

    fn open(&self, sealed: &[u8]) -> fleetglass_core::Result<Vec<u8>> {
        self.call("open", sealed)
    }
}
