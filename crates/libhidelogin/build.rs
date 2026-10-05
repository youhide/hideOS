//! The library's soname, as the recipe installs it.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libhidelogin-sd.so.0");
    }
}
