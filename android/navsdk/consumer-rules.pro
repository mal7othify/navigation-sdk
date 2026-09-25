# JNA reflects over its own classes and the generated structures.
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-dontwarn java.awt.**
# UniFFI-generated bindings are looked up by name from native code.
-keep class com.navsdk.core.** { *; }
-keep class com.navsdk.** { *; }
