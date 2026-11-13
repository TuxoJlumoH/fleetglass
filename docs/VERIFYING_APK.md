# Android Distribution and APK Signature Verification

Fleetglass provides standalone Android builds starting with v0.3. The Android app connects directly to your Kubernetes clusters without relays, accounts, or telemetry.

## System Requirements

- **Minimum Version:** Android 10+ (API level 29 or higher).
- **Rationale:** Android 10 guarantees decoupled Chromium system WebView updates (required by React 19, modern CSS Grid/container queries, ES2020+), hardware-backed Keystore AES-GCM encryption for kubeconfig credentials ([ADR 0004](adr/0004-android.md)), and TLS 1.3 by default.

## Distribution Model

Fleetglass follows an open, privacy-preserving distribution roadmap:

1. **GitHub Releases (Direct APK):**
   - Every official tag release (`v*.*.*`) publishes signed standalone APKs for both supported architectures:
     - `arm64-v8a`: Physical phones and tablets (modern 64-bit ARM devices).
     - `x86_64`: Emulators and Intel/AMD Chromebooks.
   - Every release includes:
     - Signed APK binaries.
     - `SHA256SUMS-android.txt`: Cryptographic checksums for all Android artifacts.
     - `sbom-android.json`: CycloneDX 1.5 Software Bill of Materials detailing all Cargo and npm dependencies and lockfile digests.
2. **F-Droid (Planned):**
   - Submission to F-Droid for automated reproducible builds built directly from source.
   - Fleetglass has zero proprietary dependencies and zero telemetry, making it fully compatible with F-Droid inclusion policies.
3. **Google Play Store (Under Evaluation):**
   - May be considered in the future if user demand warrants, published as an Android App Bundle (AAB) while keeping the direct APK distribution model primary.

---

## Verifying Android Artifacts

Before installing an APK on your device, you should verify both its SHA-256 checksum and its cryptographic code signature.

### 1. Verify SHA-256 Checksum

Download the APK and the `SHA256SUMS-android.txt` file from the release page, then run:

```bash
sha256sum -c SHA256SUMS-android.txt --ignore-missing
```

Or calculate the digest manually:

```bash
sha256sum fleetglass-android-arm64-v8a.apk
```

Compare the calculated 64-character hexadecimal hash with the value listed in `SHA256SUMS-android.txt`.

### 2. Verify APK Code Signature with `apksigner`

`apksigner` is part of the Android SDK Build-Tools. To verify the APK integrity and signature scheme:

```bash
apksigner verify --verbose fleetglass-android-arm64-v8a.apk
```

Expected output confirms APK Signature Scheme v2 (or v3) integrity:

```text
Verifies
Verified using v1 scheme (JAR signing): false
Verified using v2 scheme (APK Signature Scheme v2): true
Number of signers: 1
```

### 3. Verify Signer Certificate Fingerprint

To inspect the certificate that signed the APK:

```bash
apksigner verify --print-certs fleetglass-android-arm64-v8a.apk
```

Sample output:

```text
Signer #1 certificate DN: CN=Fleetglass, O=Fleetglass
Signer #1 certificate SHA-256 digest: <certificate-fingerprint>
```

Ensure the SHA-256 digest matches the official Fleetglass release signing key announced in the release notes.

---

## Installing on Android

### Via Android Debug Bridge (ADB)

With developer options and USB debugging enabled on your device:

```bash
adb install -r fleetglass-android-arm64-v8a.apk
```

### Direct Download on Phone

1. Download `fleetglass-android-arm64-v8a.apk` in your mobile browser.
2. When prompted, allow installing apps from your browser or file manager ("Install unknown apps").
3. Launch Fleetglass and import your kubeconfig file (or scan a cluster configuration QR code).
