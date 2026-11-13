//! Application view topology aggregator (`P2-05`).
//!
//! Groups projected resource rows into coherent application summaries based on
//! standard Kubernetes application labels.

use std::collections::BTreeMap;

use crate::api::{AppResource, AppSummary, Row};

/// Groups projected resource rows sharing an application name derived from
/// standard Kubernetes labels (`P2-05`).
///
/// Priority for application name resolution:
/// 1. `app.kubernetes.io/instance`
/// 2. `app.kubernetes.io/name`
/// 3. `app`
/// 4. Fallback to resource name (`row.name`)
#[must_use]
pub fn group_rows_by_app_iter<'a, I>(rows: I) -> Vec<AppSummary>
where
    I: IntoIterator<Item = &'a Row>,
{
    // Key: (app_name, namespace, cluster)
    let mut groups: BTreeMap<(String, String, String), Vec<AppResource>> = BTreeMap::new();

    for row in rows {
        let app_name = extract_app_name(row);
        let namespace = row.namespace.clone().unwrap_or_default();
        let cluster = row.cluster.clone();

        let resource = AppResource {
            cluster: cluster.clone(),
            kind: row.kind.clone(),
            namespace: namespace.clone(),
            name: row.name.clone(),
            status: row.status,
            status_reason: row.status_reason.clone(),
        };

        groups
            .entry((app_name, namespace, cluster))
            .or_default()
            .push(resource);
    }

    groups
        .into_iter()
        .map(|((name, namespace, cluster), components)| AppSummary {
            name,
            namespace,
            cluster,
            components,
        })
        .collect()
}

/// Groups projected resource rows sharing an application name derived from
/// standard Kubernetes labels (`P2-05`).
#[must_use]
pub fn group_rows_by_app(rows: &[Row]) -> Vec<AppSummary> {
    group_rows_by_app_iter(rows.iter())
}

fn extract_app_name(row: &Row) -> String {
    let find_label = |key: &str| -> Option<&str> {
        row.labels
            .iter()
            .find(|(k, v)| k == key && !v.trim().is_empty())
            .map(|(_, v)| v.as_str())
    };

    if let Some(instance) = find_label("app.kubernetes.io/instance") {
        return instance.to_owned();
    }
    if let Some(name) = find_label("app.kubernetes.io/name") {
        return name.to_owned();
    }
    if let Some(app) = find_label("app") {
        return app.to_owned();
    }
    row.name.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RowStatus;

    fn make_test_row(
        name: &str,
        kind: &str,
        namespace: Option<&str>,
        cluster: &str,
        labels: &[(&str, &str)],
    ) -> Row {
        Row {
            uid: format!("uid-{name}"),
            cluster: cluster.to_owned(),
            kind: kind.to_owned(),
            namespace: namespace.map(|s| s.to_owned()),
            name: name.to_owned(),
            status: RowStatus::Healthy,
            status_reason: "Running".to_owned(),
            cells: vec![],
            labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            custom: Vec::new(),
        }
    }

    #[test]
    fn test_label_priority_precedence() {
        // All three labels present -> instance wins
        let r1 = make_test_row(
            "pod-1",
            "Pod",
            Some("default"),
            "c1",
            &[
                ("app", "redis-legacy"),
                ("app.kubernetes.io/name", "redis"),
                ("app.kubernetes.io/instance", "cache-instance"),
            ],
        );
        assert_eq!(extract_app_name(&r1), "cache-instance");

        // name and app present -> name wins
        let r2 = make_test_row(
            "pod-2",
            "Pod",
            Some("default"),
            "c1",
            &[("app", "legacy-app"), ("app.kubernetes.io/name", "web-app")],
        );
        assert_eq!(extract_app_name(&r2), "web-app");

        // only app present -> app wins
        let r3 = make_test_row(
            "pod-3",
            "Pod",
            Some("default"),
            "c1",
            &[("app", "backend-service")],
        );
        assert_eq!(extract_app_name(&r3), "backend-service");

        // no app labels -> fallback to resource name
        let r4 = make_test_row(
            "custom-deploy",
            "Deployment",
            Some("default"),
            "c1",
            &[("tier", "frontend")],
        );
        assert_eq!(extract_app_name(&r4), "custom-deploy");
    }

    #[test]
    fn test_group_rows_by_app_grouping_and_isolation() {
        let rows = vec![
            make_test_row(
                "web-deployment",
                "Deployment",
                Some("prod"),
                "cluster-a",
                &[("app.kubernetes.io/name", "web")],
            ),
            make_test_row(
                "web-service",
                "Service",
                Some("prod"),
                "cluster-a",
                &[("app.kubernetes.io/name", "web")],
            ),
            make_test_row(
                "web-deployment",
                "Deployment",
                Some("staging"),
                "cluster-a",
                &[("app.kubernetes.io/name", "web")],
            ),
            make_test_row(
                "db-pod",
                "Pod",
                Some("prod"),
                "cluster-a",
                &[("app", "postgres")],
            ),
            make_test_row(
                "cluster-wide-cr",
                "ClusterRole",
                None,
                "cluster-a",
                &[], // no label -> app name = "cluster-wide-cr"
            ),
        ];

        let summaries = group_rows_by_app(&rows);

        // Expect 4 groups:
        // 1. cluster-wide-cr (namespace: "")
        // 2. postgres in prod
        // 3. web in prod (2 components: Deployment, Service)
        // 4. web in staging (1 component: Deployment)
        assert_eq!(summaries.len(), 4);

        let web_prod = summaries
            .iter()
            .find(|s| s.name == "web" && s.namespace == "prod")
            .expect("web in prod exists");
        assert_eq!(web_prod.cluster, "cluster-a");
        assert_eq!(web_prod.components.len(), 2);
        assert_eq!(web_prod.components[0].kind, "Deployment");
        assert_eq!(web_prod.components[0].name, "web-deployment");
        assert_eq!(web_prod.components[1].kind, "Service");
        assert_eq!(web_prod.components[1].name, "web-service");

        let web_staging = summaries
            .iter()
            .find(|s| s.name == "web" && s.namespace == "staging")
            .expect("web in staging exists");
        assert_eq!(web_staging.components.len(), 1);

        let cluster_cr = summaries
            .iter()
            .find(|s| s.name == "cluster-wide-cr")
            .expect("cluster-wide-cr exists");
        assert_eq!(cluster_cr.namespace, "");
        assert_eq!(cluster_cr.components[0].namespace, "");
    }

    #[test]
    fn test_group_rows_by_app_empty() {
        let empty: Vec<Row> = vec![];
        let res = group_rows_by_app(&empty);
        assert!(res.is_empty());
    }
}
