fn main() {
    println!("cargo:rustc-link-lib=opus");
    println!("cargo:rustc-link-lib=pulse-simple");
    println!("cargo:rustc-link-lib=pulse");
}
