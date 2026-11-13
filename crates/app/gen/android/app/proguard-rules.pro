# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile

# Fleetglass (03-02b). tauri-android's consumer rules already keep @TauriPlugin classes with their
# @Command/@ActivityCallback methods and @InvokeArg classes; these explicit rules also pin the
# names Rust uses (register_android_plugin looks the class up by name, commands by method name).
-keep class dev.fleetglass.Fleetglass.FleetglassPlugin {
  public <init>(android.app.Activity);
  @app.tauri.annotation.Command public <methods>;
  @app.tauri.annotation.ActivityCallback <methods>;
}
-keep class dev.fleetglass.Fleetglass.DataArgs { *; }