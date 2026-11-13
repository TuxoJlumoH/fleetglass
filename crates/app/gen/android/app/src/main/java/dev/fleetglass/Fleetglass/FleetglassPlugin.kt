package dev.fleetglass.Fleetglass

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.activity.result.ActivityResult
import androidx.core.content.FileProvider
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.ByteArrayOutputStream
import java.io.File
import java.security.KeyStore
import java.util.concurrent.Executors
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

@InvokeArg
class DataArgs {
  var data: String? = null
}

@InvokeArg
class ShareArgs {
  var title: String? = null
  var fileName: String? = null
  var text: String? = null
}

/**
 * Credential sealing with an Android Keystore key and the document picker for kubeconfig import
 * (task 03-02b). Registered from Rust as `dev.fleetglass.Fleetglass` / `FleetglassPlugin`.
 *
 * Tauri runs commands and activity callbacks on the main thread, so the work runs on [worker];
 * results are resolved from there. Never log payloads, file contents or key material.
 *
 * Sealed format: version byte (1) || 12-byte GCM IV || ciphertext with 16-byte tag.
 *
 * `pickFile` signals a cancelled picker by resolving `{"cancelled": true}` (not a rejection), so
 * rejections always mean a real error.
 *
 * `shareText` (task 03-04a) writes the text to `cacheDir/shared/` and offers it to other apps
 * through the `${applicationId}.fileprovider` FileProvider and the system share sheet.
 */
@TauriPlugin
class FleetglassPlugin(private val activity: Activity) : Plugin(activity) {
  // One thread: keeps Keystore key creation and file reads off the main thread and serialised.
  private val worker = Executors.newSingleThreadExecutor()

  @Command
  fun seal(invoke: Invoke) {
    val plain = decodeArg(invoke) ?: return
    worker.execute {
      try {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, getOrCreateKey())
        val iv = cipher.iv
        if (iv == null || iv.size != IV_LEN) {
          invoke.reject("seal failed: unexpected IV length from the Keystore")
          return@execute
        }
        val ct = cipher.doFinal(plain)
        val out = ByteArray(1 + IV_LEN + ct.size)
        out[0] = FORMAT_VERSION
        System.arraycopy(iv, 0, out, 1, IV_LEN)
        System.arraycopy(ct, 0, out, 1 + IV_LEN, ct.size)
        invoke.resolve(JSObject().put("data", Base64.encodeToString(out, Base64.NO_WRAP)))
      } catch (e: Exception) {
        invoke.reject("seal failed: ${e.javaClass.simpleName}")
      }
    }
  }

  @Command
  fun open(invoke: Invoke) {
    val sealed = decodeArg(invoke) ?: return
    worker.execute {
      try {
        if (sealed.size < 1 + IV_LEN + TAG_BITS / 8) {
          invoke.reject("open failed: sealed data is too short")
          return@execute
        }
        if (sealed[0] != FORMAT_VERSION) {
          invoke.reject("open failed: unsupported sealed data version ${sealed[0].toInt() and 0xff}")
          return@execute
        }
        val key = loadKey()
        if (key == null) {
          invoke.reject(
            "open failed: the credential key is missing (app data restored or key deleted); re-import the contexts"
          )
          return@execute
        }
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, sealed, 1, IV_LEN))
        val plain = cipher.doFinal(sealed, 1 + IV_LEN, sealed.size - 1 - IV_LEN)
        invoke.resolve(JSObject().put("data", Base64.encodeToString(plain, Base64.NO_WRAP)))
      } catch (e: AEADBadTagException) {
        invoke.reject(
          "open failed: authentication failed (the credential key changed, e.g. after a reinstall); re-import the contexts"
        )
      } catch (e: Exception) {
        invoke.reject("open failed: ${e.javaClass.simpleName}")
      }
    }
  }

  @Command
  fun shareText(invoke: Invoke) {
    val args = try {
      invoke.parseArgs(ShareArgs::class.java)
    } catch (e: Exception) {
      invoke.reject("invalid share arguments")
      return
    }
    val text = args.text
    if (text == null) {
      invoke.reject("missing argument: text")
      return
    }
    val title = args.title ?: ""
    val name = sanitizeFileName(args.fileName)
    worker.execute {
      try {
        val bytes = text.toByteArray(Charsets.UTF_8)
        if (bytes.size > MAX_SHARE_BYTES) {
          invoke.reject("the text to share is larger than 8 MiB")
          return@execute
        }
        val dir = File(activity.cacheDir, SHARE_DIR)
        if (!dir.isDirectory && !dir.mkdirs()) {
          invoke.reject("could not create the share cache directory")
          return@execute
        }
        pruneShared(dir)
        val file = File(dir, name)
        file.writeBytes(bytes)
        val uri = FileProvider.getUriForFile(
          activity, "${activity.packageName}.fileprovider", file
        )
        val send = Intent(Intent.ACTION_SEND).apply {
          type = "text/plain"
          putExtra(Intent.EXTRA_STREAM, uri)
          putExtra(Intent.EXTRA_SUBJECT, title)
          addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        val chooser = Intent.createChooser(send, title.ifBlank { null })
        activity.runOnUiThread {
          try {
            activity.startActivity(chooser)
            invoke.resolve()
          } catch (e: Exception) {
            invoke.reject("could not open the share sheet: ${e.javaClass.simpleName}")
          }
        }
      } catch (e: Exception) {
        invoke.reject("could not share the text: ${e.javaClass.simpleName}")
      }
    }
  }

  /** Deletes shared files older than [SHARE_MAX_AGE_MS]; other apps had time to read them. */
  private fun pruneShared(dir: File) {
    val cutoff = System.currentTimeMillis() - SHARE_MAX_AGE_MS
    dir.listFiles()?.forEach { f ->
      if (f.isFile && f.lastModified() < cutoff) f.delete()
    }
  }

  /** Keeps letters, digits, `.`, `-` and `_`; no path separators or leading dots. */
  private fun sanitizeFileName(raw: String?): String {
    val cleaned = (raw ?: "")
      .map { c -> if (c.isLetterOrDigit() || c == '.' || c == '-' || c == '_') c else '_' }
      .joinToString("")
      .trimStart('.')
      .take(MAX_SHARE_NAME_CHARS)
    return cleaned.ifBlank { "fleetglass.txt" }
  }

  @Command
  fun pickFile(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
      addCategory(Intent.CATEGORY_OPENABLE)
      type = "*/*"
    }
    startActivityForResult(invoke, intent, "pickFileResult")
  }

  @ActivityCallback
  fun pickFileResult(invoke: Invoke, result: ActivityResult) {
    val uri = result.data?.data
    if (result.resultCode != Activity.RESULT_OK || uri == null) {
      invoke.resolve(JSObject().put("cancelled", true))
      return
    }
    worker.execute {
      try {
        val (name, size) = queryNameAndSize(uri)
        if (size != null && size > MAX_FILE_BYTES) {
          invoke.reject(TOO_LARGE)
          return@execute
        }
        val data = readBounded(uri)
        if (data == null) {
          invoke.reject(TOO_LARGE)
          return@execute
        }
        val ret = JSObject()
        ret.put("cancelled", false)
        ret.put("name", name)
        ret.put("data", Base64.encodeToString(data, Base64.NO_WRAP))
        invoke.resolve(ret)
      } catch (e: Exception) {
        invoke.reject("could not read the selected file: ${e.javaClass.simpleName}")
      }
    }
  }

  private fun decodeArg(invoke: Invoke): ByteArray? {
    val b64 = try {
      invoke.parseArgs(DataArgs::class.java).data
    } catch (e: Exception) {
      null
    }
    if (b64 == null) {
      invoke.reject("missing argument: data")
      return null
    }
    return try {
      Base64.decode(b64, Base64.NO_WRAP)
    } catch (e: IllegalArgumentException) {
      invoke.reject("argument data is not valid base64")
      null
    }
  }

  private fun queryNameAndSize(uri: Uri): Pair<String?, Long?> {
    activity.contentResolver.query(
      uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null
    )?.use { c ->
      if (c.moveToFirst()) {
        val nameIdx = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
        val sizeIdx = c.getColumnIndex(OpenableColumns.SIZE)
        val name = if (nameIdx >= 0 && !c.isNull(nameIdx)) c.getString(nameIdx) else null
        val size = if (sizeIdx >= 0 && !c.isNull(sizeIdx)) c.getLong(sizeIdx) else null
        return Pair(name, size)
      }
    }
    return Pair(null, null)
  }

  /** Reads at most [MAX_FILE_BYTES]; returns null if the stream is longer (size may be unknown). */
  private fun readBounded(uri: Uri): ByteArray? {
    val input = activity.contentResolver.openInputStream(uri)
      ?: throw java.io.IOException("no input stream")
    input.use { s ->
      val out = ByteArrayOutputStream()
      val buf = ByteArray(16 * 1024)
      var total = 0L
      while (true) {
        val n = s.read(buf)
        if (n < 0) break
        total += n
        if (total > MAX_FILE_BYTES) return null
        out.write(buf, 0, n)
      }
      return out.toByteArray()
    }
  }

  private fun loadKey(): SecretKey? {
    val ks = KeyStore.getInstance(KEYSTORE).apply { load(null) }
    return ks.getKey(KEY_ALIAS, null) as? SecretKey
  }

  private fun getOrCreateKey(): SecretKey {
    loadKey()?.let { return it }
    val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
    gen.init(
      KeyGenParameterSpec.Builder(
        KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
      )
        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
        .setKeySize(256)
        .build()
    )
    return gen.generateKey()
  }

  companion object {
    private const val KEYSTORE = "AndroidKeyStore"
    private const val KEY_ALIAS = "fleetglass-credentials-v1"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val FORMAT_VERSION: Byte = 1
    private const val IV_LEN = 12
    private const val TAG_BITS = 128
    private const val MAX_FILE_BYTES = 1048576L
    private const val TOO_LARGE = "the selected file is larger than 1 MiB"
    private const val SHARE_DIR = "shared"
    private const val MAX_SHARE_BYTES = 8 * 1024 * 1024
    private const val MAX_SHARE_NAME_CHARS = 100
    private const val SHARE_MAX_AGE_MS = 60L * 60L * 1000L
  }
}
