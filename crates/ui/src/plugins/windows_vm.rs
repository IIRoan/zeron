use super::{Contribution, Manifest, PluginId};

// Like Services, the settings group and the rail entry are Shell extensions
// (`shell/worktrees_ui/windows_vm.rs`). Turning the plugin off hides them;
// a project stays connected and its agents keep their VM tools until it is
// disconnected.
pub const MANIFEST: Manifest = Manifest {
    id: PluginId::WindowsVm,
    name: "Windows VM",
    description: "A Windows machine for a project (Linux): its servers' ports reach the VM on localhost, agents run commands in it and see its screen, and its settings sit with the project's environment.",
    contributions: &[Contribution::WindowsVm],
};
