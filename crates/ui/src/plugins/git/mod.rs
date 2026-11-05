use super::{Contribution, Manifest, PluginId};

pub mod history;
pub mod source_control;

pub const MANIFEST: Manifest = Manifest {
    id: PluginId::Git,
    name: "Git",
    description: "Source control, staging, branches, stashes and commit history with syntax-highlighted diffs.",
    contributions: &[
        Contribution::SourceControl,
        Contribution::GitHistory,
        Contribution::GitCommands,
    ],
};
