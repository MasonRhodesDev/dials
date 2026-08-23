fn main() {
    let cfg =
        slint_build::CompilerConfiguration::new().with_include_paths(vec![slint_kit::ui_dir()]);
    slint_build::compile_with_config("ui/app.slint", cfg).unwrap();
}
