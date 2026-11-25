//! Per-namespace watcher fallback on RBAC 403 (05-01).
//!
//! When the cluster-wide list/watch of a namespaced kind is forbidden, the store
//! is fed by one watcher per accessible namespace instead. This module decides
//! which namespaces to watch:
//!
//! 1. the context's configured `watchNamespaces` setting, if any;
//! 2. otherwise every namespace, if the user may list namespaces;
//! 3. otherwise the kubeconfig context's namespace (or `default`) as a last resort.
//!
//! Each candidate is checked with a `SelfSubjectRulesReview` (one request per
//! namespace, any authenticated user may create one); candidates whose rules do
//! not grant both `list` and `watch` are reported as denied and not watched.

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::authorization::v1::{
    ResourceRule, SelfSubjectRulesReview, SelfSubjectRulesReviewSpec,
};
use k8s_openapi::api::core::v1::Namespace;
use kube::api::{ListParams, PostParams};

use crate::api::NamespaceSource;

/// Most namespaces one store watches separately. More candidates are cut (sorted
/// by name, first ones win) and reported as truncated, so a user who may list a
/// large cluster's namespaces but not the resource never starts thousands of watches.
pub const MAX_FALLBACK_NAMESPACES: usize = 64;

/// Concurrent `SelfSubjectRulesReview` requests while checking candidates.
const REVIEW_CONCURRENCY: usize = 8;

/// Verbs a per-namespace watcher needs.
const WATCH_VERBS: [&str; 2] = ["list", "watch"];

/// Namespaces to watch for a store whose cluster-wide watch is forbidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackPlan {
    pub source: NamespaceSource,
    /// Namespaces to watch, sorted.
    pub allowed: Vec<String>,
    /// Candidates the rules review denied, sorted.
    pub denied: Vec<String>,
    pub truncated: bool,
}

/// Candidate namespaces: sorted, deduplicated, empty names dropped, cut to
/// [`MAX_FALLBACK_NAMESPACES`]. Returns the list and whether it was cut.
#[must_use]
pub fn normalise_candidates(mut names: Vec<String>) -> (Vec<String>, bool) {
    names.retain(|n| !n.trim().is_empty());
    for n in &mut names {
        *n = n.trim().to_owned();
    }
    names.sort();
    names.dedup();
    let truncated = names.len() > MAX_FALLBACK_NAMESPACES;
    names.truncate(MAX_FALLBACK_NAMESPACES);
    (names, truncated)
}

fn covers(list: Option<&Vec<String>>, value: &str) -> bool {
    list.is_some_and(|l| l.iter().any(|v| v == "*" || v == value))
}

/// `true` when `rules` grant every verb in `verbs` on `resource` of API `group`
/// (`""` for core) without a `resourceNames` restriction (a list/watch cannot be
/// limited to names).
#[must_use]
pub fn rules_allow(rules: &[ResourceRule], group: &str, resource: &str, verbs: &[&str]) -> bool {
    verbs.iter().all(|verb| {
        rules.iter().any(|rule| {
            rule.verbs.iter().any(|v| v == "*" || v == verb)
                && covers(rule.api_groups.as_ref(), group)
                && covers(rule.resources.as_ref(), resource)
                && rule.resource_names.as_ref().is_none_or(Vec::is_empty)
        })
    })
}

/// `true` when the user may list and watch `resource` in `namespace`, by a
/// `SelfSubjectRulesReview`. An incomplete review (e.g. a webhook authorizer
/// the review cannot evaluate) that does not grant access counts as allowed:
/// the namespace's watcher then reports the real answer itself.
async fn namespace_allows(
    client: &kube::Client,
    namespace: &str,
    group: &str,
    resource: &str,
) -> Result<bool, kube::Error> {
    let review = SelfSubjectRulesReview {
        spec: SelfSubjectRulesReviewSpec {
            namespace: Some(namespace.to_owned()),
        },
        ..Default::default()
    };
    let reviewed = kube::Api::<SelfSubjectRulesReview>::all(client.clone())
        .create(&PostParams::default(), &review)
        .await?;
    let Some(status) = reviewed.status else {
        return Ok(true);
    };
    Ok(rules_allow(&status.resource_rules, group, resource, &WATCH_VERBS) || status.incomplete)
}

/// Candidate namespaces and their source (see the module docs). A forbidden
/// namespace list falls back to the client's default namespace; other errors
/// are returned.
async fn candidates(
    client: &kube::Client,
    configured: Vec<String>,
) -> Result<(Vec<String>, NamespaceSource), kube::Error> {
    if configured.iter().any(|n| !n.trim().is_empty()) {
        return Ok((configured, NamespaceSource::Configured));
    }
    let list = kube::Api::<Namespace>::all(client.clone())
        .list_metadata(&ListParams::default())
        .await;
    match list {
        Ok(list) => Ok((
            list.items
                .into_iter()
                .filter_map(|ns| ns.metadata.name)
                .collect(),
            NamespaceSource::Discovered,
        )),
        Err(kube::Error::Api(status)) if status.code == 403 => Ok((
            vec![client.default_namespace().to_owned()],
            NamespaceSource::ContextNamespace,
        )),
        Err(err) => Err(err),
    }
}

/// Works out which namespaces to watch `resource` (API `group`) in.
pub async fn plan(
    client: &kube::Client,
    group: &str,
    resource: &str,
    configured: Vec<String>,
) -> Result<FallbackPlan, kube::Error> {
    let (names, source) = candidates(client, configured).await?;
    let (names, truncated) = normalise_candidates(names);
    let checked: Vec<(String, bool)> = futures::stream::iter(names)
        .map(|ns| async move {
            let ok = namespace_allows(client, &ns, group, resource).await?;
            Ok::<_, kube::Error>((ns, ok))
        })
        .buffered(REVIEW_CONCURRENCY)
        .try_collect()
        .await?;
    let (allowed, denied): (Vec<_>, Vec<_>) = checked.into_iter().partition(|(_, ok)| *ok);
    Ok(FallbackPlan {
        source,
        allowed: allowed.into_iter().map(|(ns, _)| ns).collect(),
        denied: denied.into_iter().map(|(ns, _)| ns).collect(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(groups: &[&str], resources: &[&str], verbs: &[&str]) -> ResourceRule {
        let v = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        ResourceRule {
            api_groups: Some(v(groups)),
            resources: Some(v(resources)),
            verbs: v(verbs),
            resource_names: None,
        }
    }

    #[test]
    fn rules_allow_needs_every_verb_on_the_resource_and_group() {
        let pods_rw = rule(&[""], &["pods"], &["get", "list", "watch"]);
        assert!(rules_allow(
            std::slice::from_ref(&pods_rw),
            "",
            "pods",
            &WATCH_VERBS
        ));
        // Wrong group or resource.
        assert!(!rules_allow(
            std::slice::from_ref(&pods_rw),
            "apps",
            "pods",
            &WATCH_VERBS
        ));
        assert!(!rules_allow(&[pods_rw], "", "secrets", &WATCH_VERBS));
        // `list` without `watch` is not enough; verbs may come from separate rules.
        let list_only = rule(&[""], &["pods"], &["list"]);
        assert!(!rules_allow(
            std::slice::from_ref(&list_only),
            "",
            "pods",
            &WATCH_VERBS
        ));
        let watch_only = rule(&[""], &["pods"], &["watch"]);
        assert!(rules_allow(
            &[list_only, watch_only],
            "",
            "pods",
            &WATCH_VERBS
        ));
    }

    #[test]
    fn rules_allow_honours_wildcards_and_resource_names() {
        let admin = rule(&["*"], &["*"], &["*"]);
        assert!(rules_allow(&[admin], "apps", "deployments", &WATCH_VERBS));
        let mut named = rule(&["apps"], &["deployments"], &["list", "watch"]);
        named.resource_names = Some(vec!["web".to_owned()]);
        assert!(!rules_allow(
            &[named.clone()],
            "apps",
            "deployments",
            &WATCH_VERBS
        ));
        named.resource_names = Some(Vec::new());
        assert!(rules_allow(&[named], "apps", "deployments", &WATCH_VERBS));
        let no_groups = ResourceRule {
            api_groups: None,
            ..rule(&[], &["pods"], &["list", "watch"])
        };
        assert!(!rules_allow(&[no_groups], "", "pods", &WATCH_VERBS));
        assert!(!rules_allow(&[], "", "pods", &WATCH_VERBS));
    }

    #[test]
    fn candidates_are_sorted_deduplicated_and_capped() {
        let (names, cut) = normalise_candidates(vec![
            "prod".to_owned(),
            " dev ".to_owned(),
            String::new(),
            "dev".to_owned(),
        ]);
        assert_eq!(names, vec!["dev".to_owned(), "prod".to_owned()]);
        assert!(!cut);

        let many: Vec<String> = (0..MAX_FALLBACK_NAMESPACES + 5)
            .map(|i| format!("ns-{i:03}"))
            .collect();
        let (names, cut) = normalise_candidates(many);
        assert_eq!(names.len(), MAX_FALLBACK_NAMESPACES);
        assert_eq!(names[0], "ns-000");
        assert!(cut);
    }
}
