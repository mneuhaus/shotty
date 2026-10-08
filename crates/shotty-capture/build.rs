// ScreenCaptureKit's Swift bridge links the Swift runtime, which lives in /usr/lib/swift.
// A dependency's build script can't add rpaths to our binaries, so every binary crate says it itself.
fn main() {
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
