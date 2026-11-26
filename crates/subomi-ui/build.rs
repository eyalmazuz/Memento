fn main() {
    // Slint's compiler recurses deeply on large `.slint` files; Windows MSVC
    // defaults to a 1 MiB main-thread stack, which overflows in debug builds.
    std::thread::Builder::new()
        .name("slint-build".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            slint_build::compile("../../ui/app.slint")
                .expect("failed to compile Slint UI definitions");
        })
        .expect("failed to spawn slint-build thread")
        .join()
        .expect("slint-build thread panicked");
}
