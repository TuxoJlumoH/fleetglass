package dev.fleetglass.Fleetglass

import android.os.Bundle
import android.system.Os
import android.util.Log
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // Android does not set HOME for app processes; the Rust core derives the settings path from
    // it. Kubeconfigs are not read from files on Android: imported contexts live in the
    // Keystore-sealed store (03-02).
    try {
      Os.setenv("HOME", applicationInfo.dataDir, true)
    } catch (e: Exception) {
      Log.e("fleetglass", "setting HOME failed", e)
    }
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
  }
}
