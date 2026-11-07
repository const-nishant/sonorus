//! UniFFI binding generator (`cargo run -p uniffi-bindgen -- generate --library <lib> --language kotlin --out-dir <dir>`).

fn main() {
    uniffi::uniffi_bindgen_main()
}
