//! Embeds the application icon and manifest into `battray.exe`.

fn main() {
    println!("cargo:rerun-if-changed=res/battray.rc");
    println!("cargo:rerun-if-changed=res/battray.ico");
    println!("cargo:rerun-if-changed=res/battray.manifest");
    embed_resource::compile("res/battray.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
