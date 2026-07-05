# cutlass-ios-macos

The iOS/macOS harness app (SwiftUI) plus the `CutlassMobile` Swift package. It
links the `cutlass-mobile` `staticlib` through the plain C ABI to render engine
frames on the device GPU.

Build the prebuilt FFI bundle with `scripts/build-ios-xcframework.sh`, then open
`cutlass-ios-macos.xcodeproj` in Xcode.
