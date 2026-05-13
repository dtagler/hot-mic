fn main() {
    embed_resource::compile("app.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/icons/tray-idle.ico");
    println!("cargo:rerun-if-changed=assets/icons/tray-cam.ico");
    println!("cargo:rerun-if-changed=assets/icons/tray-mic.ico");
    println!("cargo:rerun-if-changed=assets/icons/tray-both.ico");
}
