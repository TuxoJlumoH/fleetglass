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

use std::future::Future;

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

/// Outcome of a `SelfSubjectRulesReview` for one namespace, reduced to what the
/// fallback needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RulesReview {
    pub rules: Vec<ResourceRule>,
    /// The review could not evaluate every authorizer (e.g. a webhook).
    pub incomplete: bool,
}

impl RulesReview {
    /// `true` when the review allows list + watch of `resource` (API `group`).
    /// An incomplete review that does not grant access counts as allowed: the
    /// namespace's watcher then reports the real answer itself.
    #[must_use]
    pub fn allows(&self, group: &str, resource: &str) -> bool {
        rules_allow(&self.rules, group, resource, &WATCH_VERBS) || self.incomplete
    }
}

async fn rules_review(client: &kube::Client, namespace: &str) -> Result<RulesReview, kube::Error> {
    let review = SelfSubjectRulesReview {
        spec: SelfSubjectRulesReviewSpec {
            namespace: Some(namespace.to_owned()),
        },
        ..Default::default()
    };
    let reviewed = kube::Api::<SelfSubjectRulesReview>::all(client.clone())
        .create(&PostParams::default(), &review)
        .await?;
    // No status at all: nothing to go on, let the watcher find out.
    Ok(reviewed.status.map_or(
        RulesReview {
            rules: Vec::new(),
            incomplete: true,
        },
        |status| RulesReview {
            rules: status.resource_rules,
            incomplete: status.incomplete,
        },
    ))
}

/// Candidate namespaces and their source (see the module docs), given the
/// configured list, the result of listing namespaces (only awaited when nothing
/// is configured) and the client's default namespace. A forbidden (403) list
/// falls back to the default namespace; other errors are returned.
pub async fn candidates<L>(
    configured: Vec<String>,
    list_namespaces: L,
    default_namespace: &str,
) -> Result<(Vec<String>, NamespaceSource), kube::Error>
where
    L: Future<Output = Result<Vec<String>, kube::Error>>,
{
    if configured.iter().any(|n| !n.trim().is_empty()) {
        return Ok((configured, NamespaceSource::Configured));
    }
    match list_namespaces.await {
        Ok(names) => Ok((names, NamespaceSource::Discovered)),
        Err(kube::Error::Api(status)) if status.code == 403 => Ok((
            vec![default_namespace.to_owned()],
            NamespaceSource::ContextNamespace,
        )),
        Err(err) => Err(err),
    }
}

/// [`plan`] with the API calls injected: `list_namespaces` lists namespace
/// names, `review` runs a rules review for one namespace.
pub async fn plan_with<L, R, RF>(
    group: &str,
    resource: &str,
    configured: Vec<String>,
    default_namespace: &str,
    list_namespaces: L,
    review: R,
) -> Result<FallbackPlan, kube::Error>
where
    L: Future<Output = Result<Vec<String>, kube::Error>>,
    R: Fn(String) -> RF,
    RF: Future<Output = Result<RulesReview, kube::Error>>,
{
    let (names, source) = candidates(configured, list_namespaces, default_namespace).await?;
    let (names, truncated) = normalise_candidates(names);
    let review = &review;
    let checked: Vec<(String, bool)> = futures::stream::iter(names)
        .map(|ns| async move {
            let ok = review(ns.clone()).await?.allows(group, resource);
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

/// Works out which namespaces to watch `resource` (API `group`) in.
pub async fn plan(
    client: &kube::Client,
    group: &str,
    resource: &str,
    configured: Vec<String>,
) -> Result<FallbackPlan, kube::Error> {
    let list = async {
        let list = kube::Api::<Namespace>::all(client.clone())
            .list_metadata(&ListParams::default())
            .await?;
        Ok(list
            .items
            .into_iter()
            .filter_map(|ns| ns.metadata.name)
            .collect())
    };
    plan_with(
        group,
        resource,
        configured,
        client.default_namespace(),
        list,
        |ns| async move { rules_review(client, &ns).await },
    )
    .await
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

    fn api_error(code: u16) -> kube::Error {
        kube::Error::Api(
            kube::core::Status::failure("denied", "Forbidden")
                .with_code(code)
                .boxed(),
        )
    }

    /// Rules review: `pods` list/watch only in the namespaces of `allowed`.
    fn review_for(
        allowed: &'static [&'static str],
    ) -> impl Fn(String) -> std::future::Ready<Result<RulesReview, kube::Error>> {
        move |ns: String| {
            let rules = if allowed.contains(&ns.as_str()) {
                vec![rule(&[""], &["pods"], &["get", "list", "watch"])]
            } else {
                vec![rule(&[""], &["configmaps"], &["list", "watch"])]
            };
            std::future::ready(Ok(RulesReview {
                rules,
                incomplete: false,
            }))
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[tokio::test]
    async fn discovered_namespaces_are_filtered_by_the_rules_review() {
        let plan = plan_with(
            "",
            "pods",
            Vec::new(),
            "default",
            std::future::ready(Ok(names(&["kube-system", "dev", "default", "qa"]))),
            review_for(&["dev", "qa"]),
        )
        .await
        .expect("plan");
        assert_eq!(
            plan,
            FallbackPlan {
                source: NamespaceSource::Discovered,
                allowed: names(&["dev", "qa"]),
                denied: names(&["default", "kube-system"]),
                truncated: false,
            }
        );
    }

    #[tokio::test]
    async fn configured_namespaces_skip_the_namespace_list() {
        let list = async { panic!("must not list namespaces when some are configured") };
        let plan = plan_with(
            "",
            "pods",
            names(&["qa", "dev"]),
            "default",
            list,
            review_for(&["dev"]),
        )
        .await
        .expect("plan");
        assert_eq!(plan.source, NamespaceSource::Configured);
        assert_eq!(plan.allowed, names(&["dev"]));
        assert_eq!(plan.denied, names(&["qa"]));
    }

    #[tokio::test]
    async fn forbidden_namespace_list_falls_back_to_the_context_namespace() {
        let plan = plan_with(
            "",
            "pods",
            vec![" ".to_owned()],
            "dev",
            std::future::ready(Err(api_error(403))),
            review_for(&["dev"]),
        )
        .await
        .expect("plan");
        assert_eq!(plan.source, NamespaceSource::ContextNamespace);
        assert_eq!(plan.allowed, names(&["dev"]));
        assert!(plan.denied.is_empty());
    }

    #[tokio::test]
    async fn other_list_or_review_errors_fail_the_plan() {
        let failed = plan_with(
            "",
            "pods",
            Vec::new(),
            "default",
            std::future::ready(Err(api_error(500))),
            review_for(&[]),
        )
        .await;
        assert!(matches!(failed, Err(kube::Error::Api(s)) if s.code == 500));

        let failed = plan_with(
            "",
            "pods",
            Vec::new(),
            "default",
            std::future::ready(Ok(names(&["dev"]))),
            |_ns: String| std::future::ready(Err(api_error(500))),
        )
        .await;
        assert!(failed.is_err());
    }

    #[test]
    fn incomplete_review_counts_as_allowed() {
        let review = RulesReview {
            rules: Vec::new(),
            incomplete: true,
        };
        assert!(review.allows("", "pods"));
        assert!(!RulesReview::default().allows("", "pods"));
    }
}
