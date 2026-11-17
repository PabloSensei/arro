//! The `kubectl` CLI: contexts, namespaces, the resources of a namespace, actions on
//! them, live logs and command lines for the built-in terminal. The CLI knows the
//! kubeconfig and the credentials of the user, so nothing of that is repeated here.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use base::Cancel;
use proc_sup::{CaptureError, Process, ProcessSpec, capture};
use serde::Deserialize;
use serde_json::Value;

/// A CLI call that has not answered in this long counts as a cluster that is not there:
/// `kubectl` may wait for a context that points at an unreachable server.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// How many lines of a log come before the live ones.
const LOG_TAIL: &str = "500";
/// The resources of the tree, in the order the tree shows them.
const GET_KINDS: &str =
    "pods,deployments,statefulsets,services,configmaps,secrets,ingresses,jobs,cronjobs";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterState {
    /// No `kubectl` on this machine.
    NoCli,
    /// The CLI is there, the cluster does not answer.
    Unreachable {
        reason: String,
    },
    Up {
        version: String,
    },
}

/// One context of the kubeconfig.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubeContext {
    pub name: String,
    pub cluster: String,
    /// The namespace the context starts in; empty when the file leaves it out.
    pub namespace: String,
    pub current: bool,
}

/// The kinds of the tree, in the order it lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceKind {
    Pod,
    Deployment,
    StatefulSet,
    Service,
    ConfigMap,
    Secret,
    Ingress,
    Job,
    CronJob,
}

impl ResourceKind {
    pub const ALL: [ResourceKind; 9] = [
        Self::Pod,
        Self::Deployment,
        Self::StatefulSet,
        Self::Service,
        Self::ConfigMap,
        Self::Secret,
        Self::Ingress,
        Self::Job,
        Self::CronJob,
    ];

    /// The name the tree shows.
    pub fn title(self) -> &'static str {
        match self {
            Self::Pod => "Pods",
            Self::Deployment => "Deployments",
            Self::StatefulSet => "StatefulSets",
            Self::Service => "Services",
            Self::ConfigMap => "ConfigMaps",
            Self::Secret => "Secrets",
            Self::Ingress => "Ingresses",
            Self::Job => "Jobs",
            Self::CronJob => "CronJobs",
        }
    }

    /// The word `kubectl` takes for this kind.
    pub fn plural(self) -> &'static str {
        match self {
            Self::Pod => "pods",
            Self::Deployment => "deployments",
            Self::StatefulSet => "statefulsets",
            Self::Service => "services",
            Self::ConfigMap => "configmaps",
            Self::Secret => "secrets",
            Self::Ingress => "ingresses",
            Self::Job => "jobs",
            Self::CronJob => "cronjobs",
        }
    }

    /// The name of one resource of this kind, for a question or a tab.
    pub fn one(self) -> &'static str {
        match self {
            Self::Pod => "Pod",
            Self::Deployment => "Deployment",
            Self::StatefulSet => "StatefulSet",
            Self::Service => "Service",
            Self::ConfigMap => "ConfigMap",
            Self::Secret => "Secret",
            Self::Ingress => "Ingress",
            Self::Job => "Job",
            Self::CronJob => "CronJob",
        }
    }

    /// The `kind` of a manifest, as the JSON of the API names it.
    pub fn parse(kind: &str) -> Option<Self> {
        Some(match kind {
            "Pod" => Self::Pod,
            "Deployment" => Self::Deployment,
            "StatefulSet" => Self::StatefulSet,
            "Service" => Self::Service,
            "ConfigMap" => Self::ConfigMap,
            "Secret" => Self::Secret,
            "Ingress" => Self::Ingress,
            "Job" => Self::Job,
            "CronJob" => Self::CronJob,
            _ => return None,
        })
    }
}

impl Resource {
    pub fn is_expandable(&self) -> bool {
        matches!(self.kind, ResourceKind::Secret | ResourceKind::ConfigMap) && !self.keys.is_empty()
    }
}

/// One resource of the namespace, as the tree shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub kind: ResourceKind,
    pub name: String,
    pub namespace: String,
    /// The state in one word: `Running`, `Pending`, `CrashLoopBackOff`, `Completed`.
    pub state: String,
    /// The readiness, such as `1/1`; empty when the kind has none.
    pub ready: String,
    pub restarts: u32,
    /// What the kind says about itself: the schedule of a CronJob, the hosts of an
    /// Ingress, the type of a Service.
    pub detail: String,
    /// The names of the keys of a ConfigMap or a Secret. The values are never kept.
    pub keys: Vec<String>,
    /// The containers of a Pod, for the choice of a log.
    pub containers: Vec<String>,
}

impl Resource {
    /// A line for a report: the values of secrets never reach it.
    pub fn report(&self) -> String {
        let mut text = format!(
            "{} {} {} {}",
            self.kind.title(),
            self.name,
            self.state,
            self.ready
        );
        if self.restarts > 0 {
            text.push_str(&format!(" restarts={}", self.restarts));
        }
        if !self.detail.is_empty() {
            text.push_str(&format!(" {}", self.detail));
        }
        for key in &self.keys {
            text.push_str(&format!(" key={key}"));
        }
        text
    }
}

/// The live forwarding of a port of a Pod or a Service. Dropping it ends the process, so
/// a closed window or a stopped row leaves no forwarding behind.
pub struct PortForward {
    process: Process,
    pub namespace: String,
    pub target: String,
    pub ports: String,
}

impl PortForward {
    pub fn pid(&self) -> u32 {
        self.process.pid()
    }

    /// The status of the process, `None` while it forwards.
    pub fn try_wait(&mut self) -> Option<std::process::ExitStatus> {
        self.process.try_wait().ok().flatten()
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        let _ = self.process.stop();
    }
}

#[derive(Deserialize)]
struct ConfigView {
    #[serde(rename = "current-context", default)]
    current_context: String,
    #[serde(default)]
    contexts: Vec<ConfigContext>,
}

#[derive(Deserialize)]
struct ConfigContext {
    name: String,
    #[serde(default)]
    context: ConfigContextBody,
}

#[derive(Default, Deserialize)]
struct ConfigContextBody {
    #[serde(default)]
    cluster: String,
    #[serde(default)]
    namespace: String,
}

#[derive(Deserialize)]
struct Version {
    #[serde(rename = "serverVersion")]
    server: Option<ServerVersion>,
}

#[derive(Deserialize)]
struct ServerVersion {
    #[serde(rename = "gitVersion", default)]
    version: String,
}

#[derive(Deserialize)]
struct List {
    #[serde(default)]
    items: Vec<Value>,
}

/// The `kubectl` CLI. It knows the contexts, the servers and the credentials of the user,
/// so the IDE needs no client library of its own.
#[derive(Clone, Debug)]
pub struct Kubectl {
    program: PathBuf,
    /// `None` lets the CLI read `KUBECONFIG` and the usual file.
    kubeconfig: Option<PathBuf>,
    /// `None` uses the current context of the CLI.
    context: Option<String>,
    /// `None` uses the namespace of the context, or `default`.
    namespace: Option<String>,
    /// Calls of the CLI, shared with the clones the watch holds, for the tests that count
    /// what an idle window does.
    calls: Arc<AtomicU64>,
}

impl Kubectl {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            kubeconfig: None,
            context: None,
            namespace: None,
            calls: Arc::new(AtomicU64::new(0)),
        }
    }

    /// `kubectl` from PATH.
    pub fn find() -> Option<Self> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let name = base::program::executable("kubectl");
        std::env::split_paths(&path)
            .map(|directory| directory.join(&name))
            .find(|file| file.is_file())
            .map(Self::new)
    }

    pub fn with_kubeconfig(mut self, path: Option<PathBuf>) -> Self {
        self.kubeconfig = path.filter(|path| !path.as_os_str().is_empty());
        self
    }

    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context.filter(|context| !context.is_empty());
        self
    }

    pub fn with_namespace(mut self, namespace: Option<String>) -> Self {
        self.namespace = namespace.filter(|namespace| !namespace.is_empty());
        self
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn kubeconfig(&self) -> Option<&Path> {
        self.kubeconfig.as_deref()
    }

    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    /// How many processes the CLI started. A test uses it to prove that a closed panel
    /// asks the cluster nothing.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    fn args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        if let Some(kubeconfig) = &self.kubeconfig {
            args.push("--kubeconfig".into());
            args.push(kubeconfig.clone().into());
        }
        if let Some(context) = &self.context {
            args.push("--context".into());
            args.push(context.into());
        }
        args
    }

    pub fn command(&self) -> ProcessSpec {
        ProcessSpec::new(&self.program).args(self.args())
    }

    /// The namespace words after the subcommand of a call that reads or changes one.
    fn namespace_words(&self) -> Vec<&str> {
        match &self.namespace {
            Some(namespace) => vec!["--namespace", namespace],
            None => Vec::new(),
        }
    }

    /// Stdout of a call that succeeded, else what the CLI said.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let spec = self.command().args(args);
        let cancel = Cancel::new();
        let timer = cancel.clone();
        let (done, finished) = std::sync::mpsc::channel::<()>();
        thread::spawn(move || {
            if finished.recv_timeout(CALL_TIMEOUT).is_err() {
                timer.cancel();
            }
        });
        let result = capture(&spec, &cancel);
        let _ = done.send(());
        match result {
            Ok(output) if output.status.success() => Ok(output.stdout),
            Ok(output) => Err(first_line(&output.stderr, &output.stdout)),
            Err(CaptureError::Cancelled) => Err(format!(
                "kubectl {} did not answer in {CALL_TIMEOUT:?}",
                args.join(" ")
            )),
            Err(CaptureError::Io(error)) => Err(error.to_string()),
        }
    }

    /// Like [`Self::run`], for `kubectl diff`: exit code 1 means that the file and the
    /// cluster differ, which is an answer and not an error.
    fn run_diff(&self, args: &[&str]) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let spec = self.command().args(args);
        let cancel = Cancel::new();
        let timer = cancel.clone();
        let (done, finished) = std::sync::mpsc::channel::<()>();
        thread::spawn(move || {
            if finished.recv_timeout(CALL_TIMEOUT).is_err() {
                timer.cancel();
            }
        });
        let result = capture(&spec, &cancel);
        let _ = done.send(());
        match result {
            Ok(output) if matches!(output.status.code(), Some(0 | 1)) => {
                Ok(output.stdout + &output.stderr)
            }
            Ok(output) => Err(first_line(&output.stderr, &output.stdout)),
            Err(CaptureError::Cancelled) => Err(format!(
                "kubectl {} did not answer in {CALL_TIMEOUT:?}",
                args.join(" ")
            )),
            Err(CaptureError::Io(error)) => Err(error.to_string()),
        }
    }

    pub fn state(&self) -> ClusterState {
        if !self.program.is_file() {
            return ClusterState::NoCli;
        }
        match self.run(&["version", "-o", "json"]) {
            Ok(text) => match serde_json::from_str::<Version>(text.trim()) {
                Ok(Version {
                    server: Some(server),
                }) => ClusterState::Up {
                    version: server.version,
                },
                Ok(Version { server: None }) => ClusterState::Unreachable {
                    reason: "the cluster did not answer".into(),
                },
                Err(error) => ClusterState::Unreachable {
                    reason: format!("kubectl version: {error}"),
                },
            },
            Err(reason) => ClusterState::Unreachable { reason },
        }
    }

    /// Works with a cluster that does not answer: the contexts are entries of the
    /// kubeconfig.
    pub fn contexts(&self) -> Result<Vec<KubeContext>, String> {
        let text = self.run(&["config", "view", "-o", "json"])?;
        let view: ConfigView =
            serde_json::from_str(text.trim()).map_err(|error| format!("kubeconfig: {error}"))?;
        Ok(view
            .contexts
            .into_iter()
            .map(|context| KubeContext {
                current: context.name == view.current_context,
                name: context.name,
                cluster: context.context.cluster,
                namespace: context.context.namespace,
            })
            .collect())
    }

    pub fn namespaces(&self) -> Result<Vec<String>, String> {
        let text = self.run(&["get", "namespaces", "-o", "json"])?;
        let list: List =
            serde_json::from_str(text.trim()).map_err(|error| format!("namespaces: {error}"))?;
        let mut names: Vec<String> = list
            .items
            .iter()
            .filter_map(|item| item.pointer("/metadata/name"))
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        names.sort();
        Ok(names)
    }

    /// Every kind of the tree for the namespace of the call, in one process.
    pub fn resources(&self) -> Result<Vec<Resource>, String> {
        let mut args = vec!["get", GET_KINDS, "-o", "json"];
        args.extend(self.namespace_words());
        let text = self.run(&args)?;
        let list: List =
            serde_json::from_str(text.trim()).map_err(|error| format!("resources: {error}"))?;
        let namespace = self.namespace.clone().unwrap_or_default();
        let mut resources: Vec<Resource> = list
            .items
            .iter()
            .filter_map(|item| resource_of(item, &namespace))
            .collect();
        resources.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(resources)
    }

    /// The containers of a Pod, from the full description of the Pod.
    pub fn pod_containers(&self, name: &str) -> Result<Vec<String>, String> {
        let mut args = vec!["get", "pod", name, "-o", "json"];
        args.extend(self.namespace_words());
        let text = self.run(&args)?;
        let item: Value =
            serde_json::from_str(text.trim()).map_err(|error| format!("pod {name}: {error}"))?;
        Ok(containers_of(&item))
    }

    pub fn delete(&self, kind: ResourceKind, name: &str) -> Result<(), String> {
        self.delete_named(kind.plural(), name)
    }

    /// Deletes a resource by the word `kubectl` takes for its kind, for a manifest whose
    /// kind this crate does not name itself.
    pub fn delete_named(&self, kind: &str, name: &str) -> Result<(), String> {
        let mut args = vec!["delete", kind, name];
        args.extend(self.namespace_words());
        self.run(&args).map(drop)
    }

    /// `kubectl rollout restart`, which is what a Deployment restarts with.
    pub fn restart(&self, kind: ResourceKind, name: &str) -> Result<(), String> {
        let target = format!("{}/{name}", kind.plural());
        let mut args = vec!["rollout", "restart", &target];
        args.extend(self.namespace_words());
        self.run(&args).map(drop)
    }

    pub fn scale(&self, kind: ResourceKind, name: &str, replicas: u32) -> Result<(), String> {
        let replicas = replicas.to_string();
        let target = format!("{}/{name}", kind.plural());
        let mut args = vec!["scale", &target, "--replicas", &replicas];
        args.extend(self.namespace_words());
        self.run(&args).map(drop)
    }

    pub fn describe(&self, kind: ResourceKind, name: &str) -> Result<String, String> {
        let mut args = vec!["describe", kind.plural(), name];
        args.extend(self.namespace_words());
        self.run(&args)
    }

    pub fn yaml(&self, kind: ResourceKind, name: &str) -> Result<String, String> {
        self.yaml_named(kind.plural(), name)
    }

    /// The YAML of a resource by the word `kubectl` takes for its kind.
    pub fn yaml_named(&self, kind: &str, name: &str) -> Result<String, String> {
        let target = format!("{kind}/{name}");
        let mut args = vec!["get", &target, "-o", "yaml"];
        args.extend(self.namespace_words());
        self.run(&args)
    }

    /// The value of one key of a Secret. The CLI answers the encoded map, and only the
    /// asked key is decoded here. The value is returned to the caller only; nothing this
    /// crate prints or reports carries it.
    pub fn secret_value(&self, name: &str, key: &str) -> Result<String, String> {
        let target = format!("secret/{name}");
        let mut args = vec!["get", &target, "-o", "json"];
        args.extend(self.namespace_words());
        let text = self.run(&args)?;
        let item: Value =
            serde_json::from_str(text.trim()).map_err(|error| format!("secret {name}: {error}"))?;
        let encoded = item
            .get("data")
            .and_then(|data| data.get(key))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("secret {name} has no key {key}"))?;
        decode_base64(encoded)
    }

    /// Applies one manifest through a temporary file, so that one document of a file with
    /// several is applied alone.
    pub fn apply_manifest(&self, manifest: &str) -> Result<String, String> {
        let file = TempManifest::write(manifest)?;
        let path = file.path.display().to_string();
        self.run(&["apply", "-f", &path])
    }

    /// The difference between one manifest and the cluster; an empty answer means none.
    pub fn diff_manifest(&self, manifest: &str) -> Result<String, String> {
        let file = TempManifest::write(manifest)?;
        let path = file.path.display().to_string();
        self.run_diff(&["diff", "-f", &path])
    }

    /// A live log, for a console: the last lines, then what comes, until it is stopped.
    pub fn logs(&self, pod: &str, container: Option<&str>, previous: bool) -> ProcessSpec {
        let mut spec = self
            .command()
            .args(["logs", pod, "--tail", LOG_TAIL, "--follow"]);
        if let Some(container) = container {
            spec = spec.args(["--container", container]);
        }
        if previous {
            spec = spec.arg("--previous");
        }
        for word in self.namespace_words() {
            spec = spec.arg(word);
        }
        spec
    }

    /// A shell inside a Pod, for the built-in terminal.
    pub fn exec_command_line(&self, pod: &str, shell: &str) -> String {
        let mut words = self.words();
        words.extend([
            "exec".into(),
            "--interactive".into(),
            "--tty".into(),
            pod.into(),
        ]);
        for word in self.namespace_words() {
            words.push(word.into());
        }
        words.push("--".into());
        words.push(shell.into());
        join_command_line(&words)
    }

    /// Starts `kubectl port-forward` and keeps its process alive with the returned value.
    pub fn port_forward(&self, target: &str, ports: &str) -> Result<PortForward, String> {
        let mut spec = self.command().args(["port-forward", target, ports]);
        for word in self.namespace_words() {
            spec = spec.arg(word);
        }
        let process = spec
            .start()
            .map_err(|error| format!("kubectl port-forward: {error}"))?;
        Ok(PortForward {
            process,
            namespace: self.namespace.clone().unwrap_or_else(|| "default".into()),
            target: target.to_owned(),
            ports: ports.to_owned(),
        })
    }

    fn words(&self) -> Vec<String> {
        let mut words = vec![self.program.display().to_string()];
        words.extend(
            self.args()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        words
    }
}

/// A manifest written beside the calls of the CLI and removed when the call ends.
struct TempManifest {
    path: PathBuf,
}

impl TempManifest {
    fn write(text: &str) -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "arro-kube-{}-{}.yaml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, text).map_err(|error| format!("a temporary file: {error}"))?;
        Ok(Self { path })
    }
}

impl Drop for TempManifest {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The default kubeconfig of the user, for the hint of a setting.
pub fn default_kubeconfig() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KUBECONFIG") {
        return std::env::split_paths(&path)
            .next()
            .filter(|path| !path.as_os_str().is_empty());
    }
    base::program::home_dir().map(|home| home.join(".kube").join("config"))
}

/// Standard base64, as the API of Kubernetes writes the values of a Secret. An invalid
/// character is an error rather than a guess.
fn decode_base64(text: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for byte in text.trim().bytes() {
        let value = match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' => continue,
            _ => return Err("a secret value is not base64".to_owned()),
        };
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn first_line(stderr: &str, stdout: &str) -> String {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("kubectl failed")
        .to_owned()
}

/// One item of `kubectl get ... -o json` as the tree knows it.
fn resource_of(item: &Value, namespace: &str) -> Option<Resource> {
    let kind = ResourceKind::parse(item.get("kind")?.as_str()?)?;
    let name = item.pointer("/metadata/name")?.as_str()?.to_owned();
    let namespace = item
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
        .filter(|namespace| !namespace.is_empty())
        .unwrap_or(namespace)
        .to_owned();
    let mut resource = Resource {
        kind,
        name,
        namespace,
        state: String::new(),
        ready: String::new(),
        restarts: 0,
        detail: String::new(),
        keys: Vec::new(),
        containers: Vec::new(),
    };
    match kind {
        ResourceKind::Pod => pod_state(item, &mut resource),
        ResourceKind::Deployment | ResourceKind::StatefulSet => replica_state(item, &mut resource),
        ResourceKind::Service => {
            resource.state = item
                .pointer("/spec/type")
                .and_then(Value::as_str)
                .unwrap_or("ClusterIP")
                .to_owned();
            resource.detail = item
                .pointer("/spec/clusterIP")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
        }
        ResourceKind::ConfigMap | ResourceKind::Secret => {
            resource.state = if kind == ResourceKind::Secret {
                item.get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("Opaque")
                    .to_owned()
            } else {
                "Active".to_owned()
            };
            resource.keys = keys_of(item);
            resource.keys.sort();
        }
        ResourceKind::Ingress => {
            let mut hosts: Vec<String> = item
                .pointer("/spec/rules")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|rule| rule.get("host"))
                .filter_map(Value::as_str)
                .filter(|host| !host.is_empty())
                .map(str::to_owned)
                .collect();
            hosts.sort();
            hosts.dedup();
            resource.state = "Active".to_owned();
            resource.detail = hosts.join(", ");
        }
        ResourceKind::Job => {
            let completions = item
                .pointer("/spec/completions")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            let succeeded = item
                .pointer("/status/succeeded")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let active = item
                .pointer("/status/active")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            resource.ready = format!("{succeeded}/{completions}");
            resource.state = if succeeded >= completions {
                "Completed".to_owned()
            } else if active > 0 {
                "Running".to_owned()
            } else {
                "Pending".to_owned()
            };
        }
        ResourceKind::CronJob => {
            let suspend = item
                .pointer("/spec/suspend")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            resource.state = if suspend {
                "Suspended".to_owned()
            } else {
                "Active".to_owned()
            };
            resource.detail = item
                .pointer("/spec/schedule")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
        }
    }
    Some(resource)
}

fn pod_state(item: &Value, resource: &mut Resource) {
    resource.containers = containers_of(item);
    let statuses: Vec<&Value> = item
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .map(|statuses| statuses.iter().collect())
        .unwrap_or_default();
    let ready = statuses
        .iter()
        .filter(|status| {
            status
                .get("ready")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    let total = statuses.len().max(resource.containers.len());
    if total > 0 {
        resource.ready = format!("{ready}/{total}");
    }
    resource.restarts = statuses
        .iter()
        .filter_map(|status| status.get("restartCount"))
        .filter_map(Value::as_u64)
        .sum::<u64>() as u32;
    let waiting = statuses.iter().find_map(|status| {
        status
            .pointer("/state/waiting/reason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
    });
    let terminated = statuses.iter().find_map(|status| {
        status
            .pointer("/state/terminated/reason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
    });
    resource.state = waiting
        .or(terminated)
        .or_else(|| item.pointer("/status/phase").and_then(Value::as_str))
        .unwrap_or("Unknown")
        .to_owned();
}

fn replica_state(item: &Value, resource: &mut Resource) {
    let desired = item
        .pointer("/spec/replicas")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let ready = item
        .pointer("/status/readyReplicas")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    resource.ready = format!("{ready}/{desired}");
    resource.state = if desired == 0 {
        "Scaled to zero".to_owned()
    } else if ready >= desired {
        "Running".to_owned()
    } else {
        "Pending".to_owned()
    };
}

fn containers_of(item: &Value) -> Vec<String> {
    item.pointer("/spec/containers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|container| container.get("name"))
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn keys_of(item: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    for field in ["data", "stringData"] {
        if let Some(map) = item.get(field).and_then(Value::as_object) {
            keys.extend(map.keys().cloned());
        }
    }
    keys
}

/// Quotes the way the C runtime of Windows reads a command line back into arguments.
#[cfg(not(unix))]
fn join_command_line(words: &[String]) -> String {
    let quote = |word: &String| {
        if !word.is_empty() && !word.contains([' ', '\t', '"']) {
            return word.clone();
        }
        let mut quoted = String::from("\"");
        let mut slashes = 0;
        for c in word.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    quoted.push_str(&"\\".repeat(slashes * 2 + 1));
                    quoted.push('"');
                    slashes = 0;
                }
                _ => {
                    quoted.push_str(&"\\".repeat(slashes));
                    quoted.push(c);
                    slashes = 0;
                }
            }
        }
        quoted.push_str(&"\\".repeat(slashes * 2));
        quoted.push('"');
        quoted
    };
    words.iter().map(quote).collect::<Vec<_>>().join(" ")
}

/// Quotes one argument the way `/bin/sh` reads it back.
#[cfg(unix)]
fn push_argument(line: &mut String, argument: &str) {
    if !argument.is_empty()
        && argument
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&byte))
    {
        line.push_str(argument);
        return;
    }
    line.push('\'');
    for ch in argument.chars() {
        if ch == '\'' {
            line.push_str("'\\''");
        } else {
            line.push(ch);
        }
    }
    line.push('\'');
}

/// One line for `/bin/sh -c` from the words of a command.
#[cfg(unix)]
fn join_command_line(words: &[String]) -> String {
    let mut line = String::new();
    for word in words {
        if !line.is_empty() {
            line.push(' ');
        }
        push_argument(&mut line, word);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(kind: &str, name: &str, namespace: &str) -> Value {
        json!({
            "kind": kind,
            "metadata": {"name": name, "namespace": namespace},
        })
    }

    #[test]
    fn the_state_and_the_readiness_of_a_pod_are_read() {
        let mut pod = item("Pod", "web-0", "shop");
        pod["spec"]["containers"] = json!([{"name": "web"}]);
        pod["status"] = json!({
            "phase": "Running",
            "containerStatuses": [{
                "name": "web",
                "ready": true,
                "restartCount": 2,
                "state": {"running": {}},
            }],
        });

        let resource = resource_of(&pod, "shop").expect("a pod");

        assert_eq!(resource.kind, ResourceKind::Pod);
        assert_eq!(resource.state, "Running");
        assert_eq!(resource.ready, "1/1");
        assert_eq!(resource.restarts, 2);
        assert_eq!(resource.containers, vec!["web".to_owned()]);
    }

    #[test]
    fn a_waiting_container_names_its_reason() {
        let mut pod = item("Pod", "db-0", "shop");
        pod["spec"]["containers"] = json!([{"name": "db"}]);
        pod["status"] = json!({
            "phase": "Running",
            "containerStatuses": [{
                "ready": false,
                "restartCount": 7,
                "state": {"waiting": {"reason": "CrashLoopBackOff"}},
            }],
        });

        let resource = resource_of(&pod, "shop").expect("a pod");

        assert_eq!(resource.state, "CrashLoopBackOff");
        assert_eq!(resource.ready, "0/1");
        assert_eq!(resource.restarts, 7);
    }

    #[test]
    fn a_deployment_shows_its_ready_replicas() {
        let mut deployment = item("Deployment", "web", "shop");
        deployment["spec"]["replicas"] = json!(3);
        deployment["status"]["readyReplicas"] = json!(2);

        let resource = resource_of(&deployment, "shop").expect("a deployment");

        assert_eq!(resource.ready, "2/3");
        assert_eq!(resource.state, "Pending");
    }

    #[test]
    fn a_secret_keeps_its_keys_and_no_value() {
        let mut secret = item("Secret", "db", "shop");
        secret["type"] = json!("Opaque");
        secret["data"] = json!({"password": "czNjcjN0", "user": "YWRtaW4="});

        let resource = resource_of(&secret, "shop").expect("a secret");

        assert_eq!(
            resource.keys,
            vec!["password".to_owned(), "user".to_owned()]
        );
        assert!(!resource.report().contains("czNjcjN0"));
        assert!(!resource.report().contains("s3cr3t"));
    }

    #[test]
    fn an_ingress_lists_its_hosts() {
        let mut ingress = item("Ingress", "shop", "shop");
        ingress["spec"]["rules"] = json!([{"host": "shop.example"}, {"host": "www.example"}]);

        let resource = resource_of(&ingress, "shop").expect("an ingress");

        assert_eq!(resource.detail, "shop.example, www.example");
    }

    #[test]
    fn a_cron_job_shows_its_schedule() {
        let mut cron = item("CronJob", "nightly", "shop");
        cron["spec"]["schedule"] = json!("0 3 * * *");

        let resource = resource_of(&cron, "shop").expect("a cron job");

        assert_eq!(resource.detail, "0 3 * * *");
        assert_eq!(resource.state, "Active");
    }

    #[test]
    fn the_kinds_have_the_words_the_cli_takes() {
        assert_eq!(ResourceKind::Pod.plural(), "pods");
        assert_eq!(ResourceKind::CronJob.plural(), "cronjobs");
        assert_eq!(
            ResourceKind::parse("StatefulSet"),
            Some(ResourceKind::StatefulSet)
        );
        assert_eq!(ResourceKind::parse("Namespace"), None);
    }

    #[test]
    fn a_missing_cli_is_a_state_and_starts_no_process() {
        let cli = Kubectl::new("C:/nowhere/kubectl.exe");

        assert_eq!(cli.state(), ClusterState::NoCli);
        assert_eq!(cli.calls(), 0);
    }
}
