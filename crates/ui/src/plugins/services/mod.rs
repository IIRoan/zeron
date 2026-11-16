use super::{Contribution, Manifest, PluginId};

// The panel is mounted as a Shell extension by the native host. Keeping that
// adapter in the Shell's privacy boundary avoids exposing unrelated host state.
pub const MANIFEST: Manifest = Manifest {
    id: PluginId::Services,
    name: "Services",
    description: "Project services and databases, shared worktree configuration, live output and start, stop and restart controls.",
    contributions: &[Contribution::ProjectServices],
};
