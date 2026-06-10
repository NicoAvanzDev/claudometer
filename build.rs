fn main() {
    if cfg!(target_os = "windows") {
        embed_resource::compile("resources/app.rc", embed_resource::NONE);
    }
}
