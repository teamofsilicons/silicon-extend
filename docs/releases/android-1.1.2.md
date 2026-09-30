# Silicon Extend Android 1.1.2

Android debugging is now clearly recommended as an optional setup step. The setup screen explains
that it helps agents use more apps and controls, along with app installation, logs and other
supported device features. It remains skippable and requires the device owner's local pairing.

When debugging is connected, ordinary coordinate `press` and `click` commands use Android's
debugging input path. This reaches controls such as Swiggy search that Android hides from Extend's
accessibility service and protects from its gestures. If the keyboard is visible but the focused
field is hidden, `type` uses debugging for supported text and explicitly leaves readback unverified.
An uncertain tap or typing result is never automatically replayed.

Without debugging, ordinary accessibility control continues to work. Hidden focused fields now
produce an accurate error rather than incorrectly claiming that no field has focus. Debugging does
not expose hidden controls as accessibility refs; `fill` and Jev still need visible refs.

This release updates the Android phone, tablet and TV APK to version 1.1.2 (version code 6).
CLI 1.3.1, the service, protocol and desktop applications are unchanged.
