# cutlass-android

The Android harness app (Kotlin + Jetpack Compose). It loads the `cutlass-mobile`
`cdylib` (`.so`) at runtime and calls the JNI bridge (`CutlassNative`) to render
engine frames on the device GPU.

Build the `cutlass-mobile` library for your target ABIs, then open this folder in
Android Studio.
