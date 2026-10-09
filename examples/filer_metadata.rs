use secinfra::construct_filer_metadata;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    let mut args = std::env::args().skip(1);
    let output_dir = PathBuf::from(
        args.next()
            .expect("usage: filer_metadata OUTPUT_DIR [ZIP_PATH] [THREADS]"),
    );
    let zip = args.next().filter(|arg| arg != "-").map(PathBuf::from);
    let threads = args.next().map(|arg| arg.parse::<usize>()).transpose()?;
    let stats = construct_filer_metadata(output_dir, zip, threads).await?;
    println!("{stats:?}");
    Ok(())
}
