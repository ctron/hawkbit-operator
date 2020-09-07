/*
 * Copyright (c) 2020 Red Hat Inc.
 *
 * See the NOTICE file(s) distributed with this work for additional
 * information regarding copyright ownership.
 *
 * This program and the accompanying materials are made available under the
 * terms of the Eclipse Public License 2.0 which is available at
 * http://www.eclipse.org/legal/epl-2.0
 *
 * SPDX-License-Identifier: EPL-2.0
 */

use anyhow::Result;
use kube::Api;

use keycloak_crd::Keycloak;
use kube::api::ListParams;
use std::collections::BTreeMap;

use operator_framework::selectors::ToSelector;

pub const ALL_ROLES: &[&str] = &[
    "system_admin",
    "create_target",
    "read_target",
    "read_target_security_token",
    "update_target",
    "delete_target",
    "create_repository",
    "read_repository",
    "update_repository",
    "delete_repository",
    "download_repository_artifact",
    "tenant_configuration",
    "create_rollout",
    "read_rollout",
    "update_rollout",
    "delete_rollout",
    "handle_rollout",
    "approve_rollout",
];

pub fn all_roles() -> Vec<String> {
    ALL_ROLES.iter().map(|s| s.to_string()).collect()
}

/// Find the issuer URI by looking up the Keycloak instance
pub async fn find_issuer_uri(
    keycloaks: &Api<Keycloak>,
    realm_name: String,
    instance_labels: &BTreeMap<String, String>,
) -> Result<Option<String>> {
    let instance: Option<Keycloak> = keycloaks
        .list(&ListParams {
            label_selector: Some(instance_labels.to_selector()),
            ..Default::default()
        })
        .await
        .map(|list| list.items.first().map(|i| i.clone()))?;

    Ok(instance
        .and_then(|i| i.status)
        .map(|status| status.internal_url)
        .and_then(|url| {
            if url.is_empty() {
                None
            } else {
                Some(format!("{}/auth/realms/{}", url, realm_name))
            }
        }))
}
