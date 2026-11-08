fn main() {
    // The catalogs are compiled in: rebuild when one changes.
    println!("cargo:rerun-if-changed=../../locales");
}
