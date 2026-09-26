# Release builds are not minified (see build.gradle.kts). Rules kept for when they are.
-keepattributes *Annotation*, InnerClasses
-keep,includedescriptorclasses class com.teamofsilicons.extend.**$$serializer { *; }
-keepclassmembers class com.teamofsilicons.extend.** { *** Companion; }
