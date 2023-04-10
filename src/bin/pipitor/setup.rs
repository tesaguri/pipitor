#[derive(structopt::StructOpt)]
pub struct Opt {}

pub async fn main(opt: &crate::Opt, _subopt: Opt) -> anyhow::Result<()> {
    crate::migration::main(opt, Default::default())?;
    crate::twitter_login::main(opt, Default::default()).await?;

    Ok(())
}
