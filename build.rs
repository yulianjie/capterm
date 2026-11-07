fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let resource = "assets/windows/capterm.rc";
    println!("cargo:rerun-if-changed={resource}");
    println!("cargo:rerun-if-changed=design/icon/capterm.ico");
    embed_resource::compile(resource, embed_resource::NONE)
        .manifest_required()
        .expect("embed the CapTerm application icon");
}
