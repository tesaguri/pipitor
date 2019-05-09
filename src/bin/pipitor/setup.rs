use failure::Fallible;

#[derive(structopt::StructOpt)]
pub struct Opt {}

pub async fn main(opt: &crate::Opt, _subopt: Opt) -> Fallible<()> {
    crate::migration::main(opt, Default::default())?;
    crate::twitter_login::main(opt, Default::default()).await?;
    crate::twitter_list_sync::main(opt, Default::default()).await?;

    Ok(())
}
