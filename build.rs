fn main() -> ructe::Result<()> {
    let mut r = ructe::Ructe::from_env()?;
    r.compile_templates("templates")?;
    r.statics()?.add_files("templates")
}
