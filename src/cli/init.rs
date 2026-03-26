use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum InitLegacyMode {
    Prompt,
    Manual,
}
