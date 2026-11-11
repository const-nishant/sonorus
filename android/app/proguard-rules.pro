# Sonorus release rules. Anything not listed here is shrunk and obfuscated by R8.

# JNA: the UniFFI bindings call into libjnidispatch through reflection on these classes.
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { public *; }
-dontwarn java.awt.**

# Generated UniFFI bindings (package dev.constnishant.sonorus.core). Rust looks up the functions,
# structs and callback vtables by generated names, so the whole package is kept.
-keep class dev.constnishant.sonorus.core.** { *; }

# App classes that implement core callback interfaces and are called from Rust:
#   EventListener  -> CoreEvents
#   OutputRouter   -> UsbAudioRouter
-keep class dev.constnishant.sonorus.data.CoreEvents { *; }
-keep class dev.constnishant.sonorus.data.audio.UsbAudioRouter { *; }
