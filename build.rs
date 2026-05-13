fn main() {
    embed_resource::compile("app.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=icons/tray-idle.ico");
    println!("cargo:rerun-if-changed=icons/tray-cam.ico");
    println!("cargo:rerun-if-changed=icons/tray-mic.ico");
    println!("cargo:rerun-if-changed=icons/tray-both.ico");
}
