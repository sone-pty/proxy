fn main() -> std::io::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    rt.block_on(main_loop())
}

async fn main_loop() -> std::io::Result<()> {
    Ok(())
}
