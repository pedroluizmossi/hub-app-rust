//! Kubernetes through the CLI: one `kubectl get … -o json` with the workloads and their
//! pods, and `kubectl get pods --watch-only` to read again as soon as a pod changes. Meant
//! for a local cluster (minikube, kind, k3d) in the current kubectl context.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::core::bus::Poker;
use crate::core::exec;

/// Annotation where stopping (scaling to 0) keeps how many replicas there were, so starting
/// brings the same number back.
pub const REPLICAS_ANNOTATION: &str = "miniutils.io/replicas";

/// What runs pods: a workload the app can scale (Deployment, StatefulSet), one it can only
/// restart (DaemonSet), the pods of another controller (a Job, an operator's resource) or
/// a pod with no owner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Kind {
    #[default]
    Deployment,
    StatefulSet,
    DaemonSet,
    /// Pods of another controller: `Workload::owner_kind` says which.
    Owned,
    /// A pod nobody recreates.
    Pod,
}

impl Kind {
    /// Type name for `kubectl` (`deployment/web`); `None` for the ones it doesn't address as a whole.
    pub fn resource(self) -> Option<&'static str> {
        match self {
            Kind::Deployment => Some("deployment"),
            Kind::StatefulSet => Some("statefulset"),
            Kind::DaemonSet => Some("daemonset"),
            Kind::Owned | Kind::Pod => None,
        }
    }

    /// Stopping and starting are scaling.
    pub fn scalable(self) -> bool {
        matches!(self, Kind::Deployment | Kind::StatefulSet)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Pod {
    pub name: String,
    /// Readable state, as `kubectl get pods` shows it: "Running", "CrashLoopBackOff",
    /// "OOMKilled", "ContainerCreating", "Init:Error", "Terminating"…
    pub status: String,
    /// The pod is broken (see `PROBLEMS`), not just starting.
    pub problem: bool,
    /// Every container is ready.
    pub ready: bool,
    pub restarts: u32,
    /// Why the last container restart happened ("OOMKilled", "Error"; "" = none or unknown).
    pub last_reason: String,
    /// When it started (Unix seconds; 0 = not yet).
    pub started_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Workload {
    pub namespace: String,
    pub name: String,
    pub kind: Kind,
    /// `Kind::Owned`: the controller's kind ("Job", "Cluster"…).
    pub owner_kind: String,
    /// Replicas asked for (DaemonSet: nodes scheduled; the others: the pods there are).
    pub desired: u32,
    /// Pods ready.
    pub ready: u32,
    pub pods: Vec<Pod>,
    /// Image of the first container.
    pub image: String,
    /// When it was created (Unix seconds; 0 = unknown).
    pub created_at: i64,
    /// Replicas before it was stopped from here (`REPLICAS_ANNOTATION`).
    pub saved_replicas: Option<u32>,
}

impl Workload {
    /// "ns/kind/name": identifies it between readings.
    pub fn key(&self) -> String {
        format!("{}/{:?}/{}", self.namespace, self.kind, self.name)
    }

    pub fn problem(&self) -> bool {
        self.pods.iter().any(|p| p.problem)
    }

    pub fn stopped(&self) -> bool {
        self.kind.scalable() && self.desired == 0
    }

    pub fn restarts(&self) -> u32 {
        self.pods.iter().map(|p| p.restarts).sum()
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct KubeSnapshot {
    pub ok: bool,
    /// Why the cluster couldn't be read (stopped, unreachable).
    pub error: String,
    /// The `kubectl` command doesn't exist on this machine (nothing to watch).
    pub missing: bool,
    /// kubectl's current context ("minikube").
    pub context: String,
    pub workloads: Vec<Workload>,
}

/// Container states that mean the pod is broken (not just starting).
const PROBLEMS: &[&str] = &[
    "CrashLoopBackOff",
    "ImagePullBackOff",
    "ErrImagePull",
    "InvalidImageName",
    "CreateContainerConfigError",
    "CreateContainerError",
    "RunContainerError",
    "OOMKilled",
    "Error",
    "ContainerCannotRun",
    "DeadlineExceeded",
    "Failed",
    "Evicted",
];

/// The pod watch brings the changes; this reading is the safety net.
const EVERY: Duration = Duration::from_secs(15);
/// With the window hidden the data only feeds the tray's mood.
const EVERY_HIDDEN: Duration = Duration::from_secs(60);
/// Deadline of a reading: a stopped cluster answers "connection refused" quickly, but an
/// unreachable one would hold the thread.
const CLI_TIMEOUT: Duration = Duration::from_secs(12);
/// What `kubectl` itself waits for the API server.
const REQUEST_TIMEOUT: &str = "--request-timeout=8s";

#[derive(Default)]
pub struct KubeProvider {
    last: Option<Instant>,
    /// The `kubectl get pods --watch-only` thread is alive.
    watching: Arc<AtomicBool>,
}

impl KubeProvider {
    /// Called every second while there are subscribers; returns a snapshot only when it read.
    pub fn collect(&mut self, poker: &Poker, poked: bool) -> Option<KubeSnapshot> {
        let every = if poker.visible() { EVERY } else { EVERY_HIDDEN };
        if !poked && self.last.is_some_and(|t| t.elapsed() < every) {
            return None;
        }
        self.last = Some(Instant::now());
        let context = exec::run_timeout("kubectl", &["config", "current-context"], CLI_TIMEOUT)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let args = ["get", "deployments,statefulsets,daemonsets,replicasets,pods", "--all-namespaces", "-o", "json", REQUEST_TIMEOUT];
        let snap = match exec::run_timeout("kubectl", &args, CLI_TIMEOUT) {
            Ok(out) => KubeSnapshot { ok: true, context, workloads: parse_list(&out), ..Default::default() },
            Err(e) => KubeSnapshot { missing: e.not_found(), error: brief_error(&e.to_string()), context, ..Default::default() },
        };
        // the watch only makes sense with a cluster answering; it starts again when it does
        if snap.ok && !self.watching.swap(true, Ordering::Relaxed) {
            let (p, flag) = (poker.clone(), self.watching.clone());
            let args: Vec<String> =
                ["get", "pods", "--all-namespaces", "--watch-only", "-o", "name"].map(String::from).to_vec();
            let spawned = std::thread::Builder::new()
                .name("kube-watch".into())
                .spawn(move || super::watch::follow(p, flag, "kubectl", &args, |line| line.starts_with("pod/")));
            if let Err(e) = spawned {
                log::warn!("kube: could not start the pod watcher: {e}");
                self.watching.store(false, Ordering::Relaxed);
            }
        }
        Some(snap)
    }
}

/// kubectl's error, without the noise: the cause at the end of the first line ("connection
/// refused", "i/o timeout", "Unauthorized"), which is what tells a stopped cluster from a
/// broken one.
fn brief_error(err: &str) -> String {
    let line = err.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    // a log line from client-go: `E1118 10:00:00 123 memcache.go:381] "Couldn't get …" err="Get …: connection refused"`
    let line = if line.starts_with('E') { line.split_once("] ").map_or(line, |(_, r)| r) } else { line };
    let line = line.split_once(" err=\"").map_or(line, |(_, e)| e.trim_end_matches('"'));
    let cause = line.rsplit_once(": ").map_or(line, |(_, c)| c).trim();
    crate::providers::agents::clip(if cause.is_empty() { line } else { cause }, 160)
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    path.iter().fold(v, |v, k| &v[*k]).as_str().unwrap_or_default()
}

fn u32_at(v: &Value, path: &[&str]) -> u32 {
    path.iter().fold(v, |v, k| &v[*k]).as_u64().unwrap_or(0).min(u32::MAX as u64) as u32
}

fn time_at(v: &Value, path: &[&str]) -> i64 {
    super::docker::parse_time(str_at(v, path)).unwrap_or(0)
}

/// The controller among the owners: (kind, name).
fn controller(meta: &Value) -> Option<(&str, &str)> {
    meta["ownerReferences"]
        .as_array()?
        .iter()
        .find(|o| o["controller"] == Value::Bool(true))
        .map(|o| (o["kind"].as_str().unwrap_or_default(), o["name"].as_str().unwrap_or_default()))
}

/// Output of `kubectl get deployments,statefulsets,daemonsets,replicasets,pods -o json`:
/// the workloads with their pods. Pods of a ReplicaSet go to its Deployment; pods of another
/// controller are grouped by it; a pod of nobody is its own entry. A Job's finished pods
/// are left out (they stay around until the Job is deleted, and say nothing).
pub fn parse_list(out: &str) -> Vec<Workload> {
    let Ok(v) = serde_json::from_str::<Value>(out) else { return Vec::new() };
    let items = v["items"].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut list: Vec<Workload> = Vec::new();
    let mut index: HashMap<(String, String, String), usize> = HashMap::new();
    // ReplicaSet → its Deployment
    let mut replica_sets: HashMap<(String, String), String> = HashMap::new();

    for it in items {
        let meta = &it["metadata"];
        let ns = str_at(meta, &["namespace"]).to_string();
        let name = str_at(meta, &["name"]).to_string();
        let kind = match it["kind"].as_str() {
            Some("Deployment") => Kind::Deployment,
            Some("StatefulSet") => Kind::StatefulSet,
            Some("DaemonSet") => Kind::DaemonSet,
            Some("ReplicaSet") => {
                if let Some(("Deployment", deploy)) = controller(meta) {
                    replica_sets.insert((ns, name), deploy.to_string());
                }
                continue;
            }
            _ => continue,
        };
        let desired = match kind {
            Kind::DaemonSet => u32_at(it, &["status", "desiredNumberScheduled"]),
            _ => it["spec"]["replicas"].as_u64().map_or(1, |n| n as u32),
        };
        let saved = meta["annotations"][REPLICAS_ANNOTATION].as_str().and_then(|s| s.parse().ok());
        index.insert((ns.clone(), format!("{kind:?}"), name.clone()), list.len());
        list.push(Workload {
            namespace: ns,
            name,
            kind,
            desired,
            image: it["spec"]["template"]["spec"]["containers"][0]["image"].as_str().unwrap_or_default().to_string(),
            created_at: time_at(meta, &["creationTimestamp"]),
            saved_replicas: saved,
            ..Workload::default()
        });
    }

    for it in items.iter().filter(|it| it["kind"] == "Pod") {
        let meta = &it["metadata"];
        let ns = str_at(meta, &["namespace"]).to_string();
        let pod = parse_pod(it);
        let owner = controller(meta);
        if owner.is_some_and(|(k, _)| k == "Job") && matches!(pod.status.as_str(), "Completed") {
            continue;
        }
        let (kind, owner_kind, name) = match owner {
            Some(("ReplicaSet", rs)) => match replica_sets.get(&(ns.clone(), rs.to_string())) {
                Some(deploy) => (Kind::Deployment, String::new(), deploy.clone()),
                None => (Kind::Owned, "ReplicaSet".to_string(), rs.to_string()),
            },
            Some(("StatefulSet", n)) => (Kind::StatefulSet, String::new(), n.to_string()),
            Some(("DaemonSet", n)) => (Kind::DaemonSet, String::new(), n.to_string()),
            Some((k, n)) => (Kind::Owned, k.to_string(), n.to_string()),
            None => (Kind::Pod, String::new(), pod.name.clone()),
        };
        let key = (ns.clone(), format!("{kind:?}"), name.clone());
        let i = match index.get(&key) {
            Some(&i) => i,
            None => {
                index.insert(key, list.len());
                list.push(Workload {
                    namespace: ns,
                    name,
                    kind,
                    owner_kind,
                    image: it["spec"]["containers"][0]["image"].as_str().unwrap_or_default().to_string(),
                    created_at: time_at(meta, &["creationTimestamp"]),
                    ..Workload::default()
                });
                list.len() - 1
            }
        };
        list[i].pods.push(pod);
    }

    for w in &mut list {
        w.pods.sort_by(|a, b| a.name.cmp(&b.name));
        w.ready = w.pods.iter().filter(|p| p.ready && p.status == "Running").count() as u32;
        if matches!(w.kind, Kind::Owned | Kind::Pod) {
            w.desired = w.pods.len() as u32;
        }
    }
    list
}

/// A pod, with its state in words as `kubectl get pods` puts it.
pub fn parse_pod(it: &Value) -> Pod {
    let st = &it["status"];
    let containers = st["containerStatuses"].as_array().map(Vec::as_slice).unwrap_or_default();
    let inits = st["initContainerStatuses"].as_array().map(Vec::as_slice).unwrap_or_default();
    let reason_of = |c: &Value| -> String {
        let s = &c["state"];
        let waiting = str_at(s, &["waiting", "reason"]);
        let done = str_at(s, &["terminated", "reason"]);
        if !waiting.is_empty() {
            waiting.to_string()
        } else if !done.is_empty() {
            done.to_string()
        } else if s["terminated"].is_object() {
            format!("ExitCode:{}", s["terminated"]["exitCode"].as_i64().unwrap_or(0))
        } else {
            String::new()
        }
    };
    // an init container that hasn't finished well says what the pod is doing
    let init = inits.iter().enumerate().find_map(|(i, c)| {
        let r = reason_of(c);
        match r.as_str() {
            "Completed" => None,
            "" if c["state"]["running"].is_object() => Some(format!("Init:{i}/{}", inits.len())),
            "" | "PodInitializing" => None,
            r => Some(format!("Init:{r}")),
        }
    });
    let main = containers.iter().map(reason_of).find(|r| !r.is_empty() && r != "Completed");
    let phase = str_at(st, &["phase"]);
    let status = if !it["metadata"]["deletionTimestamp"].is_null() {
        "Terminating".to_string()
    } else if let Some(r) = st["reason"].as_str().filter(|r| !r.is_empty()) {
        r.to_string() // "Evicted", "NodeLost"
    } else if let Some(i) = init {
        i
    } else if let Some(m) = main {
        m
    } else {
        match phase {
            "Succeeded" => "Completed".to_string(),
            "" => "Unknown".to_string(),
            p => p.to_string(),
        }
    };
    let problem = PROBLEMS.contains(&status.as_str())
        || status.starts_with("ExitCode:")
        || status.strip_prefix("Init:").is_some_and(|r| PROBLEMS.contains(&r) || r.starts_with("ExitCode:"));
    let last_reason = containers
        .iter()
        .map(|c| str_at(c, &["lastState", "terminated", "reason"]))
        .find(|r| !r.is_empty())
        .unwrap_or_default()
        .to_string();
    Pod {
        name: str_at(&it["metadata"], &["name"]).to_string(),
        problem,
        ready: !containers.is_empty() && containers.iter().all(|c| c["ready"] == Value::Bool(true)),
        restarts: containers.iter().map(|c| u32_at(c, &["restartCount"])).sum(),
        last_reason,
        started_at: time_at(st, &["startTime"]),
        status,
    }
}

/// Namespace and name as kubectl accepts them (DNS labels and subdomains): nothing that
/// looks like an option or carries spaces reaches the command.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && !s.starts_with(['-', '.'])
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pod(name: &str, owner: Option<(&str, &str)>, status: Value) -> Value {
        let owners = owner.map_or(json!([]), |(k, n)| json!([{"kind": k, "name": n, "controller": true}]));
        json!({"kind": "Pod", "metadata": {"namespace": "app", "name": name, "ownerReferences": owners}, "spec": {"containers": [{"image": "img:1"}]}, "status": status})
    }

    fn running(ready: bool, restarts: u32) -> Value {
        json!({"phase": "Running", "startTime": "2026-11-18T10:00:00Z",
            "containerStatuses": [{"ready": ready, "restartCount": restarts, "state": {"running": {}}}]})
    }

    fn waiting(reason: &str) -> Value {
        json!({"phase": "Running", "containerStatuses": [{"ready": false, "restartCount": 4,
            "state": {"waiting": {"reason": reason}}, "lastState": {"terminated": {"reason": "OOMKilled"}}}]})
    }

    #[test]
    fn pod_state_in_words() {
        let p = parse_pod(&pod("a", None, running(true, 0)));
        assert_eq!((p.status.as_str(), p.ready, p.problem, p.started_at > 0), ("Running", true, false, true));
        let p = parse_pod(&pod("a", None, waiting("CrashLoopBackOff")));
        assert_eq!((p.status.as_str(), p.problem, p.restarts, p.last_reason.as_str()), ("CrashLoopBackOff", true, 4, "OOMKilled"));
        // starting is not a problem
        let p = parse_pod(&pod("a", None, waiting("ContainerCreating")));
        assert_eq!((p.status.as_str(), p.problem), ("ContainerCreating", false));
        let p = parse_pod(&pod("a", None, json!({"phase": "Pending"})));
        assert_eq!((p.status.as_str(), p.problem), ("Pending", false));
        let p = parse_pod(&pod("a", None, json!({"phase": "Failed", "reason": "Evicted"})));
        assert_eq!((p.status.as_str(), p.problem), ("Evicted", true));
        let p = parse_pod(&pod("a", None, json!({"phase": "Succeeded"})));
        assert_eq!(p.status, "Completed");
        // an init container failing, or still running
        let init = |state: Value| json!({"phase": "Pending", "initContainerStatuses": [{"state": state}], "containerStatuses": [{"state": {"waiting": {"reason": "PodInitializing"}}}]});
        let p = parse_pod(&pod("a", None, init(json!({"terminated": {"reason": "Error", "exitCode": 1}}))));
        assert_eq!((p.status.as_str(), p.problem), ("Init:Error", true));
        assert_eq!(parse_pod(&pod("a", None, init(json!({"running": {}})))).status, "Init:0/1");
        // being deleted beats everything
        let mut gone = pod("a", None, running(true, 0));
        gone["metadata"]["deletionTimestamp"] = json!("2026-11-18T10:00:00Z");
        assert_eq!(parse_pod(&gone).status, "Terminating");
    }

    #[test]
    fn pods_go_to_their_workload() {
        let list = json!({"items": [
            {"kind": "Deployment", "metadata": {"namespace": "app", "name": "web", "creationTimestamp": "2026-11-18T09:00:00Z"},
             "spec": {"replicas": 2, "template": {"spec": {"containers": [{"image": "web:2"}]}}}},
            {"kind": "Deployment", "metadata": {"namespace": "app", "name": "idle", "annotations": {"miniutils.io/replicas": "3"}},
             "spec": {"replicas": 0, "template": {"spec": {"containers": [{"image": "idle:1"}]}}}},
            {"kind": "DaemonSet", "metadata": {"namespace": "kube-system", "name": "proxy"},
             "spec": {"template": {"spec": {"containers": [{"image": "proxy"}]}}}, "status": {"desiredNumberScheduled": 1}},
            {"kind": "ReplicaSet", "metadata": {"namespace": "app", "name": "web-5d8f", "ownerReferences": [{"kind": "Deployment", "name": "web", "controller": true}]}},
            pod("web-5d8f-b", Some(("ReplicaSet", "web-5d8f")), waiting("CrashLoopBackOff")),
            pod("web-5d8f-a", Some(("ReplicaSet", "web-5d8f")), running(true, 1)),
            pod("db-1", Some(("Cluster", "db")), running(true, 0)),
            pod("migrate-x", Some(("Job", "migrate")), json!({"phase": "Succeeded"})),
            pod("debug", None, running(false, 0)),
        ]});
        let w = parse_list(&list.to_string());
        let names: Vec<(&str, Kind, &str)> = w.iter().map(|w| (w.name.as_str(), w.kind, w.owner_kind.as_str())).collect();
        // the finished Job pod is left out
        assert_eq!(
            names,
            [
                ("web", Kind::Deployment, ""),
                ("idle", Kind::Deployment, ""),
                ("proxy", Kind::DaemonSet, ""),
                ("db", Kind::Owned, "Cluster"),
                ("debug", Kind::Pod, ""),
            ]
        );
        let web = &w[0];
        assert_eq!((web.desired, web.ready, web.pods.len(), web.restarts(), web.problem()), (2, 1, 2, 5, true));
        assert_eq!((web.image.as_str(), web.pods[0].name.as_str()), ("web:2", "web-5d8f-a"));
        assert!(web.created_at > 0);
        let idle = &w[1];
        assert_eq!((idle.stopped(), idle.saved_replicas, idle.pods.len()), (true, Some(3), 0));
        assert_eq!((w[2].desired, w[2].stopped()), (1, false));
        assert_eq!((w[3].desired, w[3].ready, w[3].image.as_str()), (1, 1, "img:1"));
        // not ready: counts as a pod, not as ready
        assert_eq!((w[4].desired, w[4].ready), (1, 0));
        assert!(parse_list("not json").is_empty());
    }

    #[test]
    fn names_that_reach_kubectl() {
        assert!(valid_name("ingress-nginx") && valid_name("web.v2") && valid_name("db-1"));
        assert!(!valid_name("") && !valid_name("-n") && !valid_name("a b") && !valid_name("Web") && !valid_name("a/b"));
    }

    #[test]
    fn stopped_cluster_error_in_one_line() {
        // client-go's log line, old and new
        let old = "E1118 10:00:00.000000   123 memcache.go:265] couldn't get current server API group list: Get \"https://192.168.49.2:8443/api?timeout=8s\": dial tcp 192.168.49.2:8443: connect: connection refused\nE1118 again";
        assert_eq!(brief_error(old), "connection refused");
        let new = r#"E1118 12:34:59.918038 1256596 memcache.go:381] "Couldn't get current server API group list" err="Get \"https://127.0.0.1:1/api?timeout=8s\": dial tcp 127.0.0.1:1: i/o timeout""#;
        assert_eq!(brief_error(new), "i/o timeout");
        assert_eq!(brief_error("error: You must be logged in to the server (Unauthorized)"), "You must be logged in to the server (Unauthorized)");
        assert_eq!(brief_error("The connection to the server localhost:8080 was refused"), "The connection to the server localhost:8080 was refused");
    }
}
