//! Embeds the application icon into `battray.exe`.

fn main() {
    println!("cargo:rerun-if-changed=res/battray.rc");
    println!("cargo:rerun-if-changed=res/battray.ico");
    embed_resource::compile("res/battray.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
