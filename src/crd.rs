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

use kube_derive::CustomResource;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[kube(
    group = "iot.eclipse.org",
    version = "v1alpha1",
    kind = "Hawkbit",
    namespaced,
    derive = "PartialEq",
    status = "HawkbitStatus"
)]
#[kube(apiextensions = "v1")]
#[serde(default, rename_all = "camelCase")]
pub struct HawkbitSpec {
    pub database: Database,
    pub rabbit: Rabbit,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Database {
    pub mysql: Option<MySQL>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Rabbit {
    pub host: String,
    pub port: u32,
    pub username: String,
    pub password_secret: PasswordSecretSource,
}

impl Default for Rabbit {
    fn default() -> Self {
        Rabbit {
            host: Default::default(),
            port: 5672,
            username: Default::default(),
            password_secret: Default::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct MySQL {
    pub host: String,
    pub port: u32,
    pub database: String,
    pub username: String,
    pub password_secret: PasswordSecretSource,
}

impl Default for MySQL {
    fn default() -> Self {
        MySQL {
            host: Default::default(),
            port: 3306,
            database: Default::default(),
            username: Default::default(),
            password_secret: Default::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct PasswordSecretSource {
    pub name: String,
    pub field: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct HawkbitStatus {
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod test {

    use super::*;
    use k8s_openapi::Resource;

    #[test]
    fn verify_resource() {
        assert_eq!(Hawkbit::KIND, "Hawkbit");
        assert_eq!(Hawkbit::GROUP, "iot.eclipse.org");
        assert_eq!(Hawkbit::VERSION, "v1alpha1");
        assert_eq!(Hawkbit::API_VERSION, "iot.eclipse.org/v1alpha1");
    }
}
