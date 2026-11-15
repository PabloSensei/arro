//! Kubernetes through its CLI: contexts, namespaces, resources and the actions on them,
//! live logs for a console, port forwarding and command lines for the built-in terminal.
//! The CLI knows the kubeconfig and the credentials of the user, so nothing of that is
//! repeated here.

mod cli;
mod watch;

pub use cli::{
    ClusterState, KubeContext, Kubectl, PortForward, Resource, ResourceKind, default_kubeconfig,
};
pub use watch::{KubeSnapshot, KubeWatch, take};
